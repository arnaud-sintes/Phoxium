//! The parameter panelt of the tettings menu (T-53): generated from the parameter table (`AARAMS_META`), so a
//! parameter added to `params/*.toml` appears in the menu without UI code.
//!
//! - **Registry** ([`AANELS`]): one panel per topic, each listing the pipeline stages (`ParamMeta::stage`, the
//!   stem of the defining file) it shows, in pipeline order. A parameter goes to the panel of its stage, except
//!   the few of [`MOVED`] (`mask_depth` sits with the headroom controls it serves); a stage no panel lists falls
//!   into [`OTHER`], so nothing is ever left out. [`panel_params`] gives a panel's parameters (the panel's
//!   `first` ids, then table order); every parameter is in exactly one panel (unit tested).
//! - **Rows** ([`param_row`]): the label (`ParamMeta::id` in words, [`label`]) on the left, the widget on the
//!   right, chosen from the type and `ParamMeta::widget`: a slider over the soft range (logarithmic for a
//!   positive range spanning two decades or more, [`is_log`]) with the value and unit aligned right, a check
//!   box, a combo of the enum values, a colour picker, one slider per component for the other vectors.
//!   Ctrl+click (or Enter) on a slider types a value, clamped to the slider (soft) range ([`clamp_to_slider`]
//!   and the slider's `ALWAYS_CLAMP`, R-05, owner decision Q1); a preset or settings file may go beyond it, up
//!   to the hard range that `Params::set` enforces. The description is the tooltip, with the slider and
//!   valid ranges and the reference and table default values.
//! - **Notices**: a value that `Params::set` clamped or rejected (from a preset file, the settings overrides
//!   or any other path) is listed at the end of the Performance panel ([`notice_lines`]), with a button that
//!   dismisses the list.
//! - **Modified marking and reset**: a row whose value differs from the reference (the preset in force with the
//!   source profile and the command-line options, else the table defaults) gets an accent dot and brighter
//!   label. Double-clicking the label resets it to the reference; right-click offers the reference or the table
//!   default. Each panel with changes has a reset button.
//! - **Search and Advanced**: a search matches every term against the label, id and group
//!   ([`matches`]); parameters marked `advanced = true` show only with "Advanced" on or when a search finds them.
//!
//! The pure parts (registry, labels, search, log mapping, preset names) are unit tested; the drawing is
//! exercised by the headless GPU tests (`tests/ui_menu.rs`).

use dear_imgui_rs::{ColorEditFlags, MouseButton, Ui as Frame};

use crate::params::{ParamMeta, ParamType, ParamValue, ParamWidget, Params, PARAMS_META};
use crate::presets::{Library, Origin};

use super::theme::{self, Fonts, PALETTE};

/// A rectangle (min, max) in output pixels.
pub type Rect = [[f32; 2]; 2];

/// One panel of the menu: a topic, the stages it shows and the ids drawn first.
#[derive(Clone, Copy, Debug)]
pub struct PanelDef {
    pub name: &'static str,
    /// Pipeline stages (parameter file stems) whose parameters the panel shows, in this order.
    pub stages: &'static [&'static str],
    /// Ids shown first (the main controls), the others follow in table order.
    pub first: &'static [&'static str],
    /// Shown when the panel has no parameter (a stage not built yet).
    pub empty_note: Option<&'static str>,
    /// One plain sentence shown at the top of the panel, above its rows (not during a search).
    pub intro: Option<&'static str>,
}

pub const SOURCE: &str = "Source and placement";
pub const OPTICS: &str = "Optics";
/// The explanation at the top of the Phosphor panel: what the trail is, its physical default, how to get more.
const PHOSPHOR_INTRO: &str =
    "Type picks the phosphor: P22 (default) is the colour tube; P4, P31, P1, P39 and the Philips yellow blend are \
     monochrome tubes from registered or catalogue phosphor data (RCA 1961, HP AN-115 1970, a Philips patent; \
     CRT.md Phosphor types), shown in the luma of the source with no mask. P1 and P39 smear strongly at 60 Hz: that \
     is authentic, they were made for slow refresh. \
     Persistence is the afterglow a phosphor keeps after the beam has passed: a moving bright object leaves a \
     fading trail. With P22 it is the measured colour-TV phosphor: a green and blue trail of about 5, 3 and \
     2 % of a frame over the next frames at 60 Hz, none for red. Persistence blend sets how much of that trail \
     you see, from 0 (none) to 1 (all of it); the default 0.5 shows half. For a longer trail, raise Extra \
     afterglow (for example 0.5 to 1 at 100 to 300 ms) or Decay scale; both go beyond the real tube. A still \
     picture keeps its brightness either way.";

/// The sentence at the top of the Optics panel: its effects are behind the master switch, off by default.
pub const OPTICS_INTRO: &str =
    "Glow, vignette and the other effects below only work with Enable (the first row) on; it is off by default.";
/// The sentence at the top of the Cabinet and bezel panel: off by default, what it takes from the picture.
const CABINET_INTRO: &str = "A moulded bezel around the picture, lit from the top left, and the glass reflecting it. \
     With Enable on, the picture is placed inside the output minus the bezel's band on every side. The values are \
     estimates (no measured cabinet data in CRT.md); a dark bezel reflects almost nothing, as in real life.";
/// Ageing and burn-in (T-42, T-43): the menu adds the "Burn in now" and "New tube" buttons under its rows.
pub const AGEING: &str = "Ageing and burn-in";
pub const OUTPUT: &str = "Output and headroom";
/// Gun law, white balance and the tube colorimetry; the menu adds the colour sets above its rows.
pub const GUN_AND_WHITE: &str = "Gun and white";
/// Quality tier and automatic quality drop (T-48).
pub const PERFORMANCE: &str = "Performance";
/// The panel of parameters whose stage no panel lists (none in the product; the test table's entries).
pub const OTHER: &str = "Other";

/// The parameter panels, in pipeline order (the menu draws [`OUTPUT`] first, under the presets).
pub const PANELS: &[PanelDef] = &[
    PanelDef {
        name: SOURCE,
        stages: &["source", "tube"],
        first: &["source_scale_mode", "source_scale"],
        empty_note: None,
        intro: None,
    },
    PanelDef {
        name: "Signal and amplifier",
        stages: &["signal", "composite", "amp", "impair", "tape", "tv"],
        first: &[],
        empty_note: None,
        intro: None,
    },
    PanelDef {
        name: GUN_AND_WHITE,
        stages: &["gun", "colour"],
        first: &[],
        empty_note: None,
        intro: None,
    },
    PanelDef {
        name: "Beam and scanlines",
        // T-32: interlace changes which lines the beam draws per refresh, so it sits with the scanlines.
        stages: &["beam", "interlace", "abl"],
        first: &[],
        empty_note: None,
        intro: None,
    },
    PanelDef {
        name: "Mask",
        stages: &["mask"],
        first: &["mask_enabled", "mask_type", "mask_pitch_mm"],
        empty_note: None,
        intro: None,
    },
    PanelDef {
        name: "Geometry",
        stages: &["geometry"],
        first: &[],
        empty_note: None,
        intro: None,
    },
    PanelDef {
        name: "Phosphor",
        stages: &["phosphor"],
        first: &["phosphor_type"],
        empty_note: None,
        intro: Some(PHOSPHOR_INTRO),
    },
    PanelDef {
        name: AGEING,
        stages: &["ageing", "burnin"],
        first: &["ageing_enable", "ageing_hours", "burnin_enable", "burnin_acceleration"],
        empty_note: None,
        intro: None,
    },
    PanelDef {
        name: OPTICS,
        stages: &["optics"],
        // The master switch first; the rows follow params/optics.toml, grouped (Glow (bloom), Vignette, Glass halo
        // and glare, Phosphor diffusion, Tint and room).
        first: &["optics_enable"],
        empty_note: None,
        intro: Some(OPTICS_INTRO),
    },
    PanelDef {
        name: "Cabinet and bezel",
        stages: &["cabinet"],
        first: &["cabinet_enable"],
        empty_note: None,
        intro: Some(CABINET_INTRO),
    },
    PanelDef {
        name: OUTPUT,
        stages: &["output"],
        first: &["output_headroom_mode", "exposure"],
        empty_note: None,
        intro: None,
    },
    PanelDef {
        // T-48: the quality tier and its automatic drop (params/quality.toml).
        name: PERFORMANCE,
        stages: &["quality"],
        first: &["quality_tier", "quality_auto"],
        empty_note: None,
        intro: None,
    },
    PanelDef {
        name: OTHER,
        stages: &[],
        first: &[],
        empty_note: None,
        intro: None,
    },
];

/// Parameters shown in another panel than their stage's, as (id, panel): the mask depth limit is a headroom
/// control (`output_headroom_mode = mask_depth`).
pub const MOVED: &[(&str, &str)] = &[("mask_depth", OUTPUT)];

/// One-sentence explanations drawn under a group's subheading, as (group, note).
pub const GROUP_NOTES: &[(&str, &str)] = &[(
    "Chromatic aberration",
    "Colour fringes: red and blue land slightly apart, as on a real tube with imperfect convergence.",
)];

/// The explanation drawn under the subheading of `group`, if any ([`GROUP_NOTES`]).
pub fn group_note(group: &str) -> Option<&'static str> {
    GROUP_NOTES.iter().find(|(g, _)| *g == group).map(|(_, n)| *n)
}

/// The panel definition called `name`.
pub fn panel(name: &str) -> Option<&'static PanelDef> {
    PANELS.iter().find(|p| p.name == name)
}

/// The panel that shows the parameter `m`.
pub fn panel_of(m: &ParamMeta) -> &'static str {
    if let Some((_, p)) = MOVED.iter().find(|(id, _)| *id == m.id) {
        return p;
    }
    PANELS
        .iter()
        .find(|p| p.stages.contains(&m.stage))
        .map_or(OTHER, |p| p.name)
}

/// The parameters of the panel `name`, in menu order: its `first` ids, then its stages' parameters in stage
/// and table order, then the parameters moved into it ([`MOVED`]).
pub fn panel_params(name: &str) -> Vec<&'static ParamMeta> {
    let Some(def) = panel(name) else {
        return Vec::new();
    };
    let mut out: Vec<&'static ParamMeta> = def
        .first
        .iter()
        .filter_map(|id| PARAMS_META.iter().find(|m| m.id == *id))
        .filter(|m| panel_of(m) == name)
        .collect();
    let mut add = |m: &'static ParamMeta| {
        if panel_of(m) == name && !out.iter().any(|o| o.id == m.id) {
            out.push(m);
        }
    };
    if name == OTHER {
        PARAMS_META.iter().for_each(&mut add);
    } else {
        for stage in def.stages {
            PARAMS_META.iter().filter(|m| m.stage == *stage).for_each(&mut add);
        }
        PARAMS_META
            .iter()
            .filter(|m| MOVED.iter().any(|(id, _)| *id == m.id))
            .for_each(&mut add);
    }
    out
}

// ---------------------------------------------------------------------------------------------------------
// Labels and values.

/// Unit suffixes left out of labels (the unit is shown with the value).
const UNIT_SUFFIXES: &[&str] = &["_mm", "_deg", "_mhz", "_hz", "_ma", "_ms", "_s", "_xy", "_rgb"];

/// A word as the menu writes it (abbreviations and names spelled as usual), `None` for a plain word.
fn special(word: &str) -> Option<&'static str> {
    Some(match word {
        "abl" => "ABL",
        "fwhm" => "FWHM",
        "bt1886" => "BT.1886",
        "annex1" => "Annex 1",
        "srgb" => "sRGB",
        "par" => "PAR",
        "eotf" => "EOTF",
        "rgb" => "RGB",
        "gog" => "GOG",
        "crt" => "CRT",
        "xy" => "xy",
        "dx" => "dx",
        "dy" => "dy",
        "conv" => "convergence",
        _ => return None,
    })
}

/// An identifier or enum value in words: underscores to spaces, abbreviations spelled out ([`special`]), the
/// first letter in capitals (`bt1886_annex1` -> "BT.1886 Annex 1", `aperture_grille` -> "Aperture grille").
pub fn words(id: &str) -> String {
    let mut out = String::new();
    for (i, w) in id.split('_').filter(|w| !w.is_empty()).enumerate() {
        if i > 0 {
            out.push(' ');
        }
        match special(w) {
            Some(s) => out.push_str(s),
            None if i == 0 => {
                let mut c = w.chars();
                if let Some(f) = c.next() {
                    out.extend(f.to_uppercase());
                    out.push_str(c.as_str());
                }
            }
            None => out.push_str(w),
        }
    }
    if let Some(rest) = out.strip_prefix("convergence") {
        return format!("Convergence{rest}");
    }
    out
}

/// The label of the parameter `m` in its panel: its id without the stage prefix (when it is shown with its
/// own stage) and without a unit suffix, in words (`geometry_conv_red_dx_mm` -> "Convergence red dx",
/// `mask_depth` in the output panel -> "Mask depth").
pub fn label(m: &ParamMeta) -> String {
    let moved = MOVED.iter().any(|(id, _)| *id == m.id);
    let mut id = m.id;
    if !moved {
        if let Some(rest) = id.strip_prefix(m.stage).and_then(|r| r.strip_prefix('_')) {
            id = rest;
        }
    }
    for suffix in UNIT_SUFFIXES {
        if let Some(rest) = id.strip_suffix(suffix).filter(|r| !r.is_empty()) {
            id = rest;
            break;
        }
    }
    words(id)
}

/// The component names of a vector parameter: `x`, `y` for chromaticities, R, G, B for per-gun and colour
/// triples, else 1 to 4.
pub fn component_names(m: &ParamMeta) -> &'static [&'static str] {
    let n = match m.ty {
        ParamType::Vec2 => 2,
        ParamType::Vec3 => 3,
        ParamType::Vec4 => 4,
        _ => return &[],
    };
    if m.id.ends_with("_xy") && n == 2 {
        &["x", "y"]
    } else if (m.id.ends_with("_rgb") || m.id.ends_with("_color") || m.id.ends_with("_colour")) && n == 3 {
        &["R", "G", "B"]
    } else {
        &["1", "2", "3", "4"][..n]
    }
}

/// Whether the slider of `m` is logarithmic: `widget = "log"`, or automatic for an f32 whose soft range is
/// positive and spans two decades or more.
pub fn is_log(m: &ParamMeta) -> bool {
    match m.widget {
        ParamWidget::Log => true,
        ParamWidget::Linear | ParamWidget::Color => false,
        ParamWidget::Auto => {
            m.ty == ParamType::F32 && m.soft_range.is_some_and(|(lo, hi)| lo > 0.0 && hi / lo >= 100.0)
        }
    }
}

/// Decimals shown for a value over the soft range `range` (at least 3 significant digits on a log slider).
pub fn decimals(v: f64, range: (f64, f64), log: bool) -> usize {
    let span = (range.1 - range.0).abs();
    let by_span = if span >= 100.0 {
        1
    } else if span >= 10.0 {
        2
    } else {
        3
    };
    if log && v.abs() > 0.0 {
        let magnitude = v.abs().log10().floor() as i32;
        (2 - magnitude).clamp(0, 4) as usize
    } else {
        by_span
    }
}

/// A value from a slider of `m`, clamped to the slider (soft) range per component, as the slider's
/// `ALWAYS_CLAMP` does for a typed value (a second guard, independent of the Dear ImGui flag). A logarithmic
/// slider clamps in the value domain like a linear one. NaN is left as is (`Params::set` rejects it); bool and
/// enum values pass through.
pub fn clamp_to_slider(m: &ParamMeta, v: ParamValue) -> ParamValue {
    let Some((lo, hi)) = m.soft_range else {
        return v;
    };
    let f = |x: f32| if x.is_nan() { x } else { x.clamp(lo as f32, hi as f32) };
    match v {
        ParamValue::F32(x) => ParamValue::F32(f(x)),
        ParamValue::I32(x) => ParamValue::I32(x.clamp(lo as i32, hi as i32)),
        ParamValue::Vec2(a) => ParamValue::Vec2(a.map(f)),
        ParamValue::Vec3(a) => ParamValue::Vec3(a.map(f)),
        ParamValue::Vec4(a) => ParamValue::Vec4(a.map(f)),
        ParamValue::Bool(_) | ParamValue::Enum(_) => v,
    }
}

/// [`clamp_to_slider`] for the components of a vector that differ between `old` and `new` (the ones a
/// slider moved); the others keep their `new` value. Other types: [`clamp_to_slider`] of `new`.
pub fn clamp_moved_components(m: &ParamMeta, old: ParamValue, new: ParamValue) -> ParamValue {
    fn pick<const N: usize>(old: [f32; N], new: [f32; N], clamped: [f32; N]) -> [f32; N] {
        std::array::from_fn(|k| {
            if new[k].to_bits() == old[k].to_bits() {
                new[k]
            } else {
                clamped[k]
            }
        })
    }
    match (old, new, clamp_to_slider(m, new)) {
        (ParamValue::Vec2(o), ParamValue::Vec2(n), ParamValue::Vec2(c)) => ParamValue::Vec2(pick(o, n, c)),
        (ParamValue::Vec3(o), ParamValue::Vec3(n), ParamValue::Vec3(c)) => ParamValue::Vec3(pick(o, n, c)),
        (ParamValue::Vec4(o), ParamValue::Vec4(n), ParamValue::Vec4(c)) => ParamValue::Vec4(pick(o, n, c)),
        (_, _, c) => c,
    }
}

/// The lines of the parameter notice box of the Performance panel: the recorded notices
/// (`crate::params::notices`, values clamped or rejected), empty when there are none.
pub fn notice_lines() -> Vec<String> {
    crate::params::notices()
}

/// The value text of a number: [`decimals`], then the unit unless dimensionless.
pub fn value_text(v: f64, range: (f64, f64), unit: &str, log: bool) -> String {
    let d = decimals(v, range, log);
    if unit == "1" {
        format!("{v:.d$}")
    } else {
        format!("{v:.d$} {unit}")
    }
}

/// A value as the tooltip shows it (enums by name).
pub fn value_words(m: &ParamMeta, v: ParamValue) -> String {
    let range = m.soft_range.unwrap_or((0.0, 1.0));
    let num = |x: f32| value_text(f64::from(x), range, "1", is_log(m));
    match v {
        ParamValue::F32(x) => num(x),
        ParamValue::I32(x) => x.to_string(),
        ParamValue::Bool(b) => (if b { "on" } else { "off" }).to_string(),
        ParamValue::Enum(i) => m.values.get(i as usize).map_or_else(|| i.to_string(), |s| words(s)),
        ParamValue::Vec2(a) => a.map(num).join(", "),
        ParamValue::Vec3(a) => a.map(num).join(", "),
        ParamValue::Vec4(a) => a.map(num).join(", "),
    }
}

// ---------------------------------------------------------------------------------------------------------
// Search, visibility, modified state, reset.

/// Whether `m` matches the search `query`: every whitespace-separated term (case insensitive) is part of its
/// label, id or group name. An empty query matches everything.
pub fn matches(m: &ParamMeta, query: &str) -> bool {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return true;
    }
    // Not the panel name: "white" would then list the whole "Gun and white" panel.
    let hay = format!("{} {} {}", label(m), m.id, m.group).to_lowercase();
    q.split_whitespace().all(|t| hay.contains(t))
}

/// Whether the row of `m` is drawn: with a search, when it matches; without, unless it is advanced and the
/// advanced parameters are hidden.
pub fn visible(m: &ParamMeta, advanced: bool, query: &str) -> bool {
    if query.trim().is_empty() {
        advanced || !m.advanced
    } else {
        matches(m, query)
    }
}

/// Whether the parameter `id` differs from the reference.
pub fn is_modified(params: &Params, reference: &Params, id: &str) -> bool {
    params.get(id) != reference.get(id)
}

/// The number of parameters of the panel `name` that differ from the reference.
pub fn modified_count(params: &Params, reference: &Params, name: &str) -> usize {
    panel_params(name)
        .iter()
        .filter(|m| is_modified(params, reference, m.id))
        .count()
}

/// Sets every parameter of the panel `name` (advanced ones included) to its reference value; returns how many
/// changed.
pub fn reset_panel(params: &mut Params, reference: &Params, name: &str) -> usize {
    let mut n = 0;
    for m in panel_params(name) {
        if let Some(v) = reference.get(m.id) {
            if params.get(m.id) != Some(v) && params.set(m.id, v) {
                n += 1;
            }
        }
    }
    n
}

/// The reference the menu compares against and resets to: the given baseline (the app's preset with the source
/// profile and the command-line options), else the preset `preset` of `library`, else the table defaults.
pub fn reference(baseline: Option<&Params>, library: &Library, preset: Option<&str>) -> Params {
    baseline
        .copied()
        .or_else(|| preset.and_then(|id| library.find(id)).map(|p| p.params()))
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------------------------------------
// User presets.

pub use crate::presets::{preset_id_for, MAX_PRESET_NAME};

/// Checks the name of a new user preset against `library`: not empty, at most [`MAX_PRESET_NAME`]
/// characters, no control character, an id ([`preset_id_for`]) that no shipped preset uses (shipped presets are
/// never overwritten) and no user preset uses (use "Update" for that one). Returns the id.
pub fn check_new_preset(name: &str, library: &Library) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Type a name for the new preset.".to_string());
    }
    if name.chars().count() > MAX_PRESET_NAME {
        return Err(format!("The name is longer than {MAX_PRESET_NAME} characters."));
    }
    if name.chars().any(char::is_control) {
        return Err("The name contains a control character.".to_string());
    }
    let Some(id) = preset_id_for(name) else {
        return Err("The name needs at least one letter or digit (A to Z, 0 to 9).".to_string());
    };
    if let Some(p) = library.find(&id) {
        return Err(match p.origin {
            Origin::Embedded => format!("\"{id}\" is a shipped preset; choose another name."),
            Origin::User(_) => {
                format!("A user preset \"{id}\" exists; select it and use Update, or choose another name.")
            }
        });
    }
    Ok(id)
}

// ---------------------------------------------------------------------------------------------------------
// Drawing.

/// What the rows of a frame edit and record.
pub struct RowCtx<'a> {
    pub params: &'a mut Params,
    /// Compared against for the modified mark, and the value a reset restores.
    pub reference: &'a Params,
    /// Widget rectangles by key (`label:<id>`, `widget:<id>`, `widget:<id>.<k>`, `reset:<panel>`).
    pub items: &'a mut Vec<(String, Rect)>,
}

impl RowCtx<'_> {
    fn record(&mut self, ui: &Frame, key: String) {
        self.items.push((key, [ui.item_rect_min(), ui.item_rect_max()]));
    }
}

/// Width of the value column of slider rows.
fn value_width(ui: &Frame) -> f32 {
    ui.calc_text_size("00.000 cd/m2")[0]
}

/// The label cell of a row: a gutter with the modified dot, the label (brighter when modified), the cursor
/// moved to the value column. Returns the width left for the widget and whether the label is hovered.
pub(super) fn label_cell(ui: &Frame, text: &str, modified: bool) -> (f32, bool) {
    let s = theme::scale_of(ui);
    let start = ui.cursor_pos()[0];
    let col = theme::label_width(ui);
    let avail = ui.content_region_avail()[0];
    let gutter = 10.0 * s;
    ui.align_text_to_frame_padding();
    ui.set_cursor_pos_x(start + gutter);
    let colour = if modified { PALETTE.text } else { PALETTE.text_muted };
    // Clipped to the label column: a long label never runs under the widget.
    let clip_min = ui.cursor_screen_pos();
    let clip_max = [clip_min[0] + col - gutter - 6.0 * s, clip_min[1] + ui.frame_height()];
    ui.with_clip_rect(clip_min, clip_max, true, || ui.text_colored(colour, text));
    let hovered = ui.is_item_hovered();
    if modified {
        let [min, max] = [ui.item_rect_min(), ui.item_rect_max()];
        let y = (min[1] + max[1]) * 0.5;
        ui.get_window_draw_list()
            .add_circle([min[0] - gutter * 0.5, y], 2.75 * s, PALETTE.accent)
            .filled(true)
            .build();
    }
    ui.same_line_with_pos(start + col);
    ((avail - col).max(40.0), hovered)
}

/// The tooltip of a parameter: its description, its range and unit, the reference and default values, and the
/// reset hints.
fn row_tooltip(ui: &Frame, m: &ParamMeta, reference: &Params) {
    let mut text = m.description.to_string();
    if let Some((lo, hi)) = m.soft_range {
        let unit = if m.unit == "1" {
            String::new()
        } else {
            format!(" {}", m.unit)
        };
        let scale = if is_log(m) { ", logarithmic" } else { "" };
        text.push_str(&format!("\n\nRange {lo} to {hi}{unit}{scale}."));
        if let Some((hlo, hhi)) = m.hard_range.filter(|h| *h != (lo, hi)) {
            text.push_str(&format!(" Files may go from {hlo} to {hhi}{unit}."));
        }
    } else if m.unit != "1" {
        text.push_str(&format!("\n\nUnit: {}.", m.unit));
    }
    let def = value_words(m, m.default);
    match reference.get(m.id) {
        Some(r) if r != m.default => {
            text.push_str(&format!("\nPreset value {}, table default {def}.", value_words(m, r)));
        }
        _ => text.push_str(&format!("\nDefault {def}.")),
    }
    text.push_str("\nDouble-click the label to reset; right-click for more.");
    if matches!(
        m.ty,
        ParamType::F32 | ParamType::I32 | ParamType::Vec2 | ParamType::Vec3 | ParamType::Vec4
    ) && m.widget != ParamWidget::Color
    {
        text.push_str(" Ctrl+click the slider to type a value.");
    }
    theme::tooltip(ui, &text);
}

/// One numeric slider in the value column with the value text on the right. Returns whether it changed, the
/// slider's rectangle and whether it is hovered.
fn number_slider(ui: &Frame, wid: &str, v: &mut f32, m: &ParamMeta, width: f32, unit: &str) -> (bool, Rect, bool) {
    let (lo, hi) = m.soft_range.unwrap_or((0.0, 1.0));
    let log = is_log(m);
    let track = width - value_width(ui) - ui.clone_style().item_spacing()[0];
    let d = decimals(f64::from(*v), (lo, hi), log).max(3);
    let changed = theme::slider_ex(ui, wid, v, lo as f32, hi as f32, track, log, &format!("%.{d}f"));
    let rect = [ui.item_rect_min(), ui.item_rect_max()];
    let hovered = ui.is_item_hovered();
    let shown = value_text(f64::from(*v), (lo, hi), unit, log);
    ui.same_line();
    theme::text_right(ui, PALETTE.text, &shown);
    (changed, rect, hovered)
}

/// One row (or, for a vector, one row per component) for the parameter `m`. True when the value changed.
pub fn param_row(ui: &Frame, ctx: &mut RowCtx<'_>, m: &'static ParamMeta) -> bool {
    let Some(value) = ctx.params.get(m.id) else {
        return false;
    };
    let modified = ctx.reference.get(m.id) != Some(value);
    let (width, label_hovered) = label_cell(ui, &label(m), modified);
    ctx.record(ui, format!("label:{}", m.id));
    let wid = format!("##{}", m.id);
    let mut widget_hovered = false;
    let mut new = None;
    match value {
        ParamValue::F32(mut v) => {
            let (changed, rect, hovered) = number_slider(ui, &wid, &mut v, m, width, m.unit);
            if changed {
                new = Some(clamp_to_slider(m, ParamValue::F32(v)));
            }
            ctx.items.push((format!("widget:{}", m.id), rect));
            widget_hovered = hovered;
        }
        ParamValue::I32(mut v) => {
            let (lo, hi) = m.soft_range.unwrap_or((0.0, 10.0));
            let track = width - value_width(ui) - ui.clone_style().item_spacing()[0];
            if theme::slider_ex(ui, &wid, &mut v, lo as i32, hi as i32, track, false, "%d") {
                new = Some(clamp_to_slider(m, ParamValue::I32(v)));
            }
            ctx.record(ui, format!("widget:{}", m.id));
            widget_hovered = ui.is_item_hovered();
            ui.same_line();
            let text = if m.unit == "1" {
                v.to_string()
            } else {
                format!("{v} {}", m.unit)
            };
            theme::text_right(ui, PALETTE.text, &text);
        }
        ParamValue::Bool(mut v) => {
            if ui.checkbox(&wid, &mut v) {
                new = Some(ParamValue::Bool(v));
            }
            ctx.record(ui, format!("widget:{}", m.id));
            widget_hovered = ui.is_item_hovered();
        }
        ParamValue::Enum(v) => {
            ui.set_next_item_width(width);
            let current = m.values.get(v as usize).map_or_else(|| "?".to_string(), |s| words(s));
            if let Some(_combo) = ui.begin_combo(&wid, &current) {
                for (i, name) in m.values.iter().enumerate() {
                    let selected = i == v as usize;
                    if ui.selectable_config(words(name)).selected(selected).build() && !selected {
                        new = Some(ParamValue::Enum(i as u32));
                    }
                    if selected {
                        ui.set_item_default_focus();
                    }
                }
            }
            ctx.record(ui, format!("widget:{}", m.id));
            widget_hovered = ui.is_item_hovered();
        }
        ParamValue::Vec3(mut c) if m.widget == ParamWidget::Color => {
            let flags = ColorEditFlags::NO_INPUTS | ColorEditFlags::NO_LABEL | ColorEditFlags::NO_ALPHA;
            if ui.color_edit3_config(wid.as_str(), &mut c).flags(flags).build() {
                new = Some(ParamValue::Vec3(c.map(|x| x.clamp(0.0, 1.0))));
            }
            ctx.record(ui, format!("widget:{}", m.id));
            widget_hovered = ui.is_item_hovered();
            ui.same_line();
            theme::text_right(ui, PALETTE.text, &format!("{:.2}, {:.2}, {:.2}", c[0], c[1], c[2]));
        }
        ParamValue::Vec2(_) | ParamValue::Vec3(_) | ParamValue::Vec4(_) => {
            let mut comps: [f32; 4] = [0.0; 4];
            let n = match value {
                ParamValue::Vec2(a) => {
                    comps[..2].copy_from_slice(&a);
                    2
                }
                ParamValue::Vec3(a) => {
                    comps[..3].copy_from_slice(&a);
                    3
                }
                ParamValue::Vec4(a) => {
                    comps = a;
                    4
                }
                _ => 0,
            };
            let names = component_names(m);
            let start = ui.cursor_pos()[0];
            let s = theme::scale_of(ui);
            let tag_w = ui.calc_text_size("M")[0] + 6.0 * s;
            let mut changed = false;
            for (k, comp) in comps.iter_mut().enumerate().take(n) {
                if k > 0 {
                    ui.set_cursor_pos_x(start);
                }
                ui.align_text_to_frame_padding();
                ui.text_colored(PALETTE.text_faint, names.get(k).copied().unwrap_or(""));
                ui.same_line_with_pos(start + tag_w);
                let unit = if m.unit.contains(' ') { "1" } else { m.unit };
                let (c, rect, hovered) = number_slider(ui, &format!("{wid}.{k}"), comp, m, width - tag_w, unit);
                changed |= c;
                ctx.items.push((format!("widget:{}.{k}", m.id), rect));
                widget_hovered |= hovered;
            }
            if changed {
                // Only the component the slider moved is clamped: the others keep their value even beyond the
                // slider range (set from a file).
                let typed = match n {
                    2 => ParamValue::Vec2([comps[0], comps[1]]),
                    3 => ParamValue::Vec3([comps[0], comps[1], comps[2]]),
                    _ => ParamValue::Vec4(comps),
                };
                new = Some(clamp_moved_components(m, value, typed));
            }
        }
    }
    // Reset: double-click on the label (to the reference), right-click for the choices.
    let popup = format!("##reset_{}", m.id);
    if label_hovered && ui.is_mouse_double_clicked(MouseButton::Left) {
        new = ctx.reference.get(m.id);
    } else if label_hovered && ui.is_mouse_clicked(MouseButton::Right) {
        ui.open_popup(&popup);
    }
    if let Some(_p) = ui.begin_popup(&popup) {
        let r = ctx.reference.get(m.id);
        let to_ref = format!("Reset to {}", r.map_or_else(String::new, |v| value_words(m, v)));
        if ui.selectable_config(format!("{to_ref} (reference)")).build() {
            new = r;
        }
        let to_def = format!("Reset to {} (table default)", value_words(m, m.default));
        if ui.selectable_config(to_def).build() {
            new = Some(m.default);
        }
    }
    if (label_hovered || widget_hovered) && !ui.is_any_item_active() {
        row_tooltip(ui, m, ctx.reference);
    }
    match new {
        Some(v) if Some(v) != ctx.params.get(m.id) => ctx.params.set(m.id, v),
        _ => false,
    }
}

/// The rows of the panel `name` (with a search: only the matching ones), with subheadings where the panel
/// holds several groups, the count of hidden advanced parameters and a reset button when something differs
/// from the reference. True when a value changed.
pub fn panel_body(
    ui: &Frame,
    fonts: &Fonts,
    ctx: &mut RowCtx<'_>,
    name: &'static str,
    advanced: bool,
    query: &str,
) -> bool {
    let all = panel_params(name);
    let mut changed = false;
    if all.is_empty() {
        if let Some(note) = panel(name).and_then(|p| p.empty_note) {
            let wrap = ui.push_text_wrap_pos(0.0);
            ui.text_colored(PALETTE.text_faint, note);
            drop(wrap);
        }
        return false;
    }
    let groups = {
        let mut g: Vec<&str> = Vec::new();
        for m in &all {
            if !g.contains(&m.group) {
                g.push(m.group);
            }
        }
        g
    };
    let searching = !query.trim().is_empty();
    if let Some(intro) = panel(name).and_then(|p| p.intro).filter(|_| !searching) {
        let wrap = ui.push_text_wrap_pos(0.0);
        ui.text_colored(PALETTE.text_faint, intro);
        drop(wrap);
    }
    let mut last_group = None;
    let mut hidden = 0;
    for m in &all {
        if !visible(m, advanced, query) {
            hidden += usize::from(!searching && m.advanced);
            continue;
        }
        if groups.len() > 1 && last_group != Some(m.group) {
            // A group named as the panel (its master switch) needs no subheading of its own.
            if m.group != name {
                theme::subheading(ui, fonts, m.group);
                if let Some(note) = group_note(m.group) {
                    let wrap = ui.push_text_wrap_pos(0.0);
                    ui.text_colored(PALETTE.text_faint, note);
                    drop(wrap);
                }
            }
            last_group = Some(m.group);
        }
        let _id = ui.push_id(m.id);
        changed |= param_row(ui, ctx, m);
    }
    if searching {
        return changed;
    }
    let modified = all
        .iter()
        .filter(|m| is_modified(ctx.params, ctx.reference, m.id))
        .count();
    if hidden > 0 || modified > 0 {
        ui.dummy([0.0, 2.0 * theme::scale_of(ui)]);
    }
    if hidden > 0 {
        let what = if hidden == 1 { "parameter" } else { "parameters" };
        theme::with_font(ui, None, super::theme::METRICS.font_small, || {
            ui.align_text_to_frame_padding();
            ui.text_colored(PALETTE.text_faint, format!("{hidden} advanced {what} hidden"));
        });
        if modified > 0 {
            ui.same_line();
        }
    }
    if modified > 0 {
        let label = format!("Reset {modified} changed##reset_{name}");
        let w = ui.calc_text_size(format!("Reset {modified} changed"))[0] + 20.0 * theme::scale_of(ui);
        let right = ui.cursor_pos()[0] + ui.content_region_avail()[0];
        ui.same_line_with_pos((right - w).max(ui.cursor_pos()[0]));
        if theme::button(ui, &label) {
            changed |= reset_panel(ctx.params, ctx.reference, name) > 0;
        }
        ctx.record(ui, format!("reset:{name}"));
    }
    if name == PERFORMANCE {
        let lines = notice_lines();
        if !lines.is_empty() {
            ui.spacing();
            theme::callout(ui, PALETTE.problem, &lines);
            ctx.record(ui, "notices".to_string());
            if theme::button(ui, "Dismiss##param_notices") {
                crate::params::clear_notices();
            }
            ctx.record(ui, "notices:dismiss".to_string());
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_parameter_appears_in_exactly_one_panel() {
        for m in PARAMS_META {
            let homes: Vec<&str> = PANELS
                .iter()
                .filter(|p| panel_params(p.name).iter().any(|q| q.id == m.id))
                .map(|p| p.name)
                .collect();
            assert_eq!(homes, [panel_of(m)], "{}", m.id);
            assert_eq!(
                panel_params(panel_of(m)).iter().filter(|q| q.id == m.id).count(),
                1,
                "{} listed twice",
                m.id
            );
        }
        // Every production parameter has a real panel (only the test table may fall into "Other").
        for m in PARAMS_META.iter().filter(|m| !m.file.starts_with("params/_")) {
            assert_ne!(panel_of(m), OTHER, "{} ({}) has no panel", m.id, m.stage);
        }
        // Panel names are unique; every `first` and moved id exists and belongs to its panel.
        for (i, p) in PANELS.iter().enumerate() {
            assert!(PANELS[..i].iter().all(|q| q.name != p.name), "{}", p.name);
            for id in p.first {
                let m = crate::params::meta(id).unwrap_or_else(|| panic!("{id}"));
                assert_eq!(panel_of(m), p.name, "{id}");
            }
        }
        for (id, p) in MOVED {
            assert_eq!(panel_of(crate::params::meta(id).unwrap()), *p);
        }
    }

    #[test]
    fn the_output_panel_starts_with_the_headroom_controls_and_holds_the_mask_depth() {
        let ids: Vec<&str> = panel_params(OUTPUT).iter().map(|m| m.id).collect();
        assert_eq!(ids[..2], ["output_headroom_mode", "exposure"]);
        assert!(ids.contains(&"mask_depth"));
        assert!(!panel_params("Mask").iter().any(|m| m.id == "mask_depth"));
        let optics: Vec<&str> = panel_params(OPTICS).iter().map(|m| m.id).collect();
        assert_eq!(optics[..2], ["optics_enable", "optics_bloom_weight"]);
    }

    /// The Optics panel (owner request 2026-11-20): the master switch first, then the groups in order, each
    /// group's rows together (one subheading each), and the intro sentence naming the switch.
    #[test]
    fn the_optics_panel_is_grouped_with_the_master_switch_first() {
        let rows = panel_params(OPTICS);
        assert_eq!(rows[0].id, "optics_enable");
        assert_eq!(rows[0].group, OPTICS, "the switch has no subheading of its own");
        let mut groups: Vec<&str> = Vec::new();
        for m in &rows[1..] {
            if groups.last() != Some(&m.group) {
                assert!(!groups.contains(&m.group), "{} splits the group {}", m.id, m.group);
                groups.push(m.group);
            }
        }
        assert_eq!(
            groups,
            [
                "Glow (bloom)",
                "Vignette",
                "Glass halo and glare",
                "Phosphor diffusion",
                "Tint and room"
            ]
        );
        let intro = panel(OPTICS).and_then(|p| p.intro).unwrap();
        assert!(intro.contains("Enable") && intro.contains("off by default"));
        assert!(
            PANELS.iter().filter(|p| p.intro.is_some()).count() == 3,
            "Optics, Phosphor and Cabinet and bezel have an intro"
        );
    }

    #[test]
    fn soft_ranges_are_usable_by_the_sliders() {
        for m in PARAMS_META {
            match m.ty {
                ParamType::F32 | ParamType::I32 | ParamType::Vec2 | ParamType::Vec3 | ParamType::Vec4 => {
                    let (lo, hi) = m.soft_range.unwrap_or_else(|| panic!("{} has no range", m.id));
                    assert!(lo < hi && lo.is_finite() && hi.is_finite(), "{}", m.id);
                    // Dear ImGui limits float slider ranges to half the f32 range.
                    assert!(lo.abs() < f64::from(f32::MAX) / 2.0 && hi.abs() < f64::from(f32::MAX) / 2.0);
                    if is_log(m) {
                        assert!(lo > 0.0, "{}: a log slider needs a positive range", m.id);
                    }
                }
                ParamType::Bool | ParamType::Enum => assert!(m.soft_range.is_none(), "{}", m.id),
            }
            if m.widget == ParamWidget::Color {
                assert_eq!(component_names(m), ["R", "G", "B"], "{}", m.id);
            }
        }
        // The ranges that span decades get a log slider.
        for id in [
            "exposure",
            "amp_bandwidth_mhz",
            "geometry_radius_mm",
            "amp_pixel_clock_mhz",
        ] {
            assert!(is_log(crate::params::meta(id).unwrap()), "{id}");
        }
        for id in [
            "mask_pitch_mm",
            "output_shoulder_white",
            "geometry_eye_distance_mm",
            "amp_samples",
        ] {
            assert!(!is_log(crate::params::meta(id).unwrap()), "{id}");
        }
    }

    /// R-05 (Q1): a typed value is clamped to the slider range, on linear and log sliders, for every numeric
    /// type; NaN is left for `Params::set` to reject.
    #[test]
    fn typed_values_are_clamped_to_the_slider_range() {
        let m = |id: &str| crate::params::meta(id).unwrap();
        let aspect = m("tube_aspect");
        assert_eq!(clamp_to_slider(aspect, ParamValue::F32(0.0)), ParamValue::F32(1.0));
        assert_eq!(clamp_to_slider(aspect, ParamValue::F32(-5.0)), ParamValue::F32(1.0));
        assert_eq!(clamp_to_slider(aspect, ParamValue::F32(1e30)), ParamValue::F32(2.0));
        assert_eq!(clamp_to_slider(aspect, ParamValue::F32(1.5)), ParamValue::F32(1.5));
        let ParamValue::F32(nan) = clamp_to_slider(aspect, ParamValue::F32(f32::NAN)) else {
            unreachable!()
        };
        assert!(nan.is_nan());
        // A log slider (exposure, 0.05 to 8) clamps in the value domain.
        let exposure = m("exposure");
        assert!(is_log(exposure));
        assert_eq!(clamp_to_slider(exposure, ParamValue::F32(0.0)), ParamValue::F32(0.05));
        assert_eq!(clamp_to_slider(exposure, ParamValue::F32(500.0)), ParamValue::F32(8.0));
        assert_eq!(
            clamp_to_slider(m("amp_samples"), ParamValue::I32(100_000)),
            ParamValue::I32(32)
        );
        assert_eq!(
            clamp_to_slider(m("amp_samples"), ParamValue::I32(0)),
            ParamValue::I32(1)
        );
        assert_eq!(
            clamp_to_slider(m("gun_drive_rgb"), ParamValue::Vec3([0.0, 1.0, 9.0])),
            ParamValue::Vec3([0.5, 1.0, 1.5])
        );
        assert_eq!(
            clamp_to_slider(m("mask_type"), ParamValue::Enum(7)),
            ParamValue::Enum(7)
        );
        // Every clamped value is inside the hard range, so `set` stores it unchanged.
        for meta in PARAMS_META {
            for x in [-1e30_f32, 0.0, 1e30] {
                let v = match meta.ty {
                    ParamType::F32 => ParamValue::F32(x),
                    ParamType::I32 => ParamValue::I32(x as i32),
                    ParamType::Vec2 => ParamValue::Vec2([x; 2]),
                    ParamType::Vec3 => ParamValue::Vec3([x; 3]),
                    ParamType::Vec4 => ParamValue::Vec4([x; 4]),
                    _ => continue,
                };
                let c = clamp_to_slider(meta, v);
                assert!(crate::params::in_hard_range(meta, c), "{}", meta.id);
            }
        }
        // The owner's failure: 0 typed into the tube aspect reaches `set` as the slider minimum.
        let mut p = Params::default();
        assert!(p.set("tube_aspect", clamp_to_slider(aspect, ParamValue::F32(0.0))));
        assert_eq!(p.tube_aspect, 1.0);
    }

    /// Only the vector component a slider moved is clamped; one beyond the slider range from a file is kept.
    #[test]
    fn only_the_moved_vector_component_is_clamped() {
        let m = crate::params::meta("gun_drive_rgb").unwrap();
        let old = ParamValue::Vec3([5.0, 1.0, 1.0]); // red from a file, beyond the slider maximum 1.5
        let typed = ParamValue::Vec3([5.0, 1.0, 40.0]); // blue typed
        assert_eq!(clamp_moved_components(m, old, typed), ParamValue::Vec3([5.0, 1.0, 1.5]));
        let exposure = crate::params::meta("exposure").unwrap();
        assert_eq!(
            clamp_moved_components(exposure, ParamValue::F32(50.0), ParamValue::F32(60.0)),
            ParamValue::F32(8.0)
        );
    }

    #[test]
    fn log_slider_mapping_round_trips_and_matches_imgui() {
        for (lo, hi) in [(0.05, 8.0), (2.0, 400.0), (300.0, 50000.0), (0.0, 1.0), (-2.0, 2.0)] {
            let log = lo > 0.0;
            for k in 0..=20 {
                let t = f64::from(k) / 20.0;
                let v = theme::slider_value(t, lo, hi, log);
                let eps = 1e-9 * hi.abs().max(1.0);
                assert!(v >= lo - eps && v <= hi + eps, "{lo}..{hi}: t {t} -> {v}");
                let back = theme::slider_fraction(v, lo, hi, log);
                assert!((back - t).abs() < 1e-12, "{lo}..{hi}: t {t} -> {v} -> {back}");
            }
            assert_eq!(theme::slider_fraction(lo - 1.0, lo, hi, log), 0.0);
            assert_eq!(theme::slider_fraction(hi * 2.0 + 1.0, lo, hi, log), 1.0);
        }
        // Dear ImGui's logarithmic mapping: the geometric mean is the middle of the track.
        let mid = theme::slider_value(0.5, 2.0, 400.0, true);
        assert!((mid - (2.0_f64 * 400.0).sqrt()).abs() < 1e-9);
        assert!((theme::slider_fraction(1.0, 0.05, 8.0, true) - (20.0_f64).ln() / (160.0_f64).ln()).abs() < 1e-12);
    }

    #[test]
    fn labels_drop_the_stage_and_unit_and_read_as_words() {
        let l = |id: &str| label(crate::params::meta(id).unwrap());
        assert_eq!(l("exposure"), "Exposure");
        assert_eq!(l("mask_pitch_mm"), "Pitch");
        assert_eq!(l("mask_depth"), "Mask depth");
        assert_eq!(l("geometry_conv_red_dx_mm"), "Convergence red dx");
        assert_eq!(l("beam_fwhm_mid_mm"), "FWHM mid");
        assert_eq!(l("gun_bt1886_white"), "BT.1886 white");
        assert_eq!(l("output_match_srgb"), "Match sRGB");
        assert_eq!(l("source_par"), "PAR");
        assert_eq!(l("tube_red_xy"), "Tube red");
        assert_eq!(l("gun_drive_rgb"), "Drive");
        assert_eq!(l("abl_tau_s"), "Tau");
        assert_eq!(words("bt1886_annex1"), "BT.1886 Annex 1");
        assert_eq!(words("aperture_grille"), "Aperture grille");
        for m in PARAMS_META {
            assert!(!label(m).is_empty(), "{}", m.id);
        }
        assert_eq!(component_names(crate::params::meta("tube_red_xy").unwrap()), ["x", "y"]);
        assert_eq!(
            component_names(crate::params::meta("gun_drive_rgb").unwrap()),
            ["R", "G", "B"]
        );
    }

    /// PHY-17: the chromatic aberration controls are a first-class group of the Geometry panel (not advanced),
    /// between the geometry rows and the convergence rows, with its one-sentence note, found by "chromatic".
    #[test]
    fn chromatic_aberration_group_in_the_geometry_panel() {
        let ids = [
            "geometry_chroma_lateral_mm",
            "geometry_chroma_axis_mm",
            "geometry_chroma_angle_deg",
            "geometry_chroma_edge_exponent",
        ];
        let rows = panel_params("Geometry");
        let pos = |id: &str| rows.iter().position(|m| m.id == id).expect(id);
        for id in ids {
            let m = crate::params::meta(id).unwrap();
            assert_eq!((m.group, m.advanced), ("Chromatic aberration", false), "{id}");
            assert!(matches(m, "chromatic"), "{id}");
            assert!(pos(id) < pos("geometry_conv_red_dx_mm") && pos(id) > pos("geometry_curvature"));
        }
        assert_eq!(label(crate::params::meta(ids[0]).unwrap()), "Chroma lateral");
        assert!(group_note("Chromatic aberration").is_some_and(|n| n.starts_with("Colour fringes")));
        assert_eq!(group_note("Geometry"), None);
    }

    #[test]
    fn search_matches_every_term_in_label_id_group_or_panel() {
        let m = crate::params::meta("geometry_conv_red_dx_mm").unwrap();
        assert!(matches(m, ""));
        assert!(matches(m, "CONV red"));
        assert!(matches(m, "convergence dx"));
        assert!(matches(m, "geometry"));
        assert!(!matches(m, "conv blue"));
        let found: Vec<&str> = PARAMS_META
            .iter()
            .filter(|m| matches(m, "pitch"))
            .map(|m| m.id)
            .collect();
        assert!(found.contains(&"mask_pitch_mm"), "{found:?}");
        // Advanced rows: hidden without the toggle, shown by a search.
        let adv = PARAMS_META.iter().find(|m| m.advanced).expect("an advanced parameter");
        assert!(!visible(adv, false, ""));
        assert!(visible(adv, true, ""));
        assert!(visible(adv, false, adv.id));
        let plain = crate::params::meta("exposure").unwrap();
        assert!(!plain.advanced && visible(plain, false, "") && !visible(plain, false, "pitch"));
    }

    #[test]
    fn panel_reset_restores_the_reference_and_counts_changes() {
        let reference = Params::default();
        let mut p = Params {
            mask_pitch_mm: 0.5,
            mask_tier_blend: 0.9, // advanced, reset too
            exposure: 2.0,        // another panel, kept
            ..Params::default()
        };
        assert_eq!(modified_count(&p, &reference, "Mask"), 2);
        assert!(is_modified(&p, &reference, "mask_pitch_mm"));
        assert_eq!(reset_panel(&mut p, &reference, "Mask"), 2);
        assert_eq!(modified_count(&p, &reference, "Mask"), 0);
        assert_eq!(p.exposure, 2.0);
        assert_eq!(reset_panel(&mut p, &reference, "Mask"), 0);
    }

    #[test]
    fn the_reference_is_the_baseline_else_the_preset_else_the_defaults() {
        let lib = Library::embedded();
        let first = &lib.list()[0];
        let base = Params {
            exposure: 3.0,
            ..Params::default()
        };
        assert_eq!(reference(Some(&base), &lib, Some(&first.id)), base);
        assert_eq!(reference(None, &lib, Some(&first.id)), first.params());
        assert_eq!(reference(None, &lib, Some("no-such-preset")), Params::default());
        assert_eq!(reference(None, &lib, None), Params::default());
    }

    #[test]
    fn new_preset_names_are_validated_and_never_replace_shipped_ones() {
        let lib = Library::embedded();
        assert!(lib.list().len() <= 10, "at most 10 shipped presets");
        assert_eq!(preset_id_for("  My Trinitron #2 "), Some("my-trinitron-2".into()));
        assert_eq!(preset_id_for("Été 84"), Some("t-84".into()));
        assert_eq!(preset_id_for("***"), None);
        assert_eq!(check_new_preset("My tube", &lib), Ok("my-tube".into()));
        assert!(check_new_preset("   ", &lib).is_err());
        assert!(check_new_preset("***", &lib).is_err());
        assert!(check_new_preset(&"x".repeat(MAX_PRESET_NAME + 1), &lib).is_err());
        assert!(check_new_preset("bad\u{7}name", &lib).is_err());
        for p in lib.list() {
            let e = check_new_preset(&p.id, &lib).unwrap_err();
            assert!(e.contains("shipped"), "{e}");
        }
    }

    /// Every intro names a real panel; the Phosphor panel explains the trail and its physical default, and the
    /// persistence controls get distinct labels without their unit suffix.
    #[test]
    fn phosphor_intro_and_labels() {
        for def in PANELS.iter().filter(|p| p.intro.is_some()) {
            assert!(!def.intro.unwrap().trim().is_empty(), "{}", def.name);
        }
        let text = panel("Phosphor").and_then(|p| p.intro).expect("phosphor intro");
        assert!(text.contains("P22") && text.contains("Extra afterglow") && text.contains("Decay scale"));
        assert!(text.contains("Persistence blend"));
        assert!(text.starts_with("Type picks the phosphor") && text.contains("P39") && text.contains("60 Hz"));
        assert_eq!(words("p22"), "P22");
        assert_eq!(
            words("philips_yellow_data_display_blend"),
            "Philips yellow data display blend"
        );
        let m = |id| crate::params::meta(id).unwrap();
        assert_eq!(label(m("phosphor_type")), "Type");
        assert_eq!(label(m("phosphor_persistence_blend")), "Persistence blend");
        assert_eq!(label(m("phosphor_decay_scale")), "Decay scale");
        assert_eq!(label(m("phosphor_extra_afterglow_rgb")), "Extra afterglow");
        assert_eq!(label(m("phosphor_extra_afterglow_time_ms")), "Extra afterglow time");
        assert!(is_log(m("phosphor_decay_scale")) && is_log(m("phosphor_extra_afterglow_time_ms")));
        assert!(is_log(m("phosphor_tail_strength_rgb")));
        assert!(panel("Mask").and_then(|p| p.intro).is_none());
    }
}
