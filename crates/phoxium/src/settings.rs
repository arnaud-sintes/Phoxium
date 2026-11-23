//! Persistent settings (T-51, UX-2, PRF-4, SRC-2): `%APPDATA%\Phoxium\settings.toml`.
//!
//! Holds the global settings (preset name, parameter overrides, hotkey chords, UI state) and the source profiles
//! keyed by executable file name and window class.
//! The overrides are applied onto the settings preset at start; the settings menu records in them only the ids
//! the user changed, against the preset in force ([`record_edit`], T-53). [`overrides_from_params`] gives the
//! ids that differ from the table defaults.
//!
//! Writes are atomic (a temporary file named per process, flushed, then renamed) and run on a background thread
//! ([`Writer`]), never on the render thread. A file that cannot be used gives defaults plus a warning
//! ([`load`]): an invalid file is kept as `settings.toml.bak` (the first copy is never overwritten) and is then
//! replaced by the session's settings; an unreadable file or one of a newer schema is never written by the
//! session ([`Loaded::writable`]), so a newer build's settings survive a run of an older one.

use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::params::{ParamType, ParamValue, Params, PARAMS_META};
use crate::zerocopy::CapturePath;

/// Schema version written to the file. Files with a higher version are not understood and are set aside.
pub const SCHEMA_VERSION: u32 = 1;
pub const SETTINGS_FILE: &str = "settings.toml";
pub const BACKUP_FILE: &str = "settings.toml.bak";

/// Hotkey action ids with their default chords (T-54; parsed and validated by `hotkeys::Bindings::from_settings`).
/// The menu key is F10 (PLAN D-17: F12 is owned by another program on the dev machine). No default uses Ctrl+Alt
/// with a character key: those fire on AltGr on European keyboard layouts (review D-01).
pub const DEFAULT_HOTKEYS: &[(&str, &str)] = &[
    ("quit", "Ctrl+Alt+End"),
    ("toggle_fullscreen", "F11"),
    ("toggle_menu", "F10"),
    ("toggle_overlay", "Ctrl+Alt+F9"),
    ("toggle_bypass", "Ctrl+Alt+F7"),
    ("next_preset", "Ctrl+Alt+PageDown"),
    ("previous_preset", "Ctrl+Alt+PageUp"),
    ("toggle_click_through", "Ctrl+Alt+F6"),
    ("snapshot", "Ctrl+Alt+F2"),
    ("hold_bypass", "Ctrl+Alt+Insert"),
];

/// The generation of the default hotkeys a file was written with (`[global] hotkey_defaults`): 1 (the key is
/// absent) for the files of earlier builds, which wrote every chord and whose defaults were Ctrl+Alt+Q, O and B;
/// 2 since review D-01. A file of generation 1 has its old defaults upgraded at load ([`Settings::from_toml`]).
pub const HOTKEY_DEFAULTS: u32 = 2;

fn legacy_hotkey_defaults() -> u32 {
    1
}

/// Writes `[global.hotkeys]` without the chords equal to their default: only what the user changed is stored, so
/// a later change of a default reaches everyone who kept it.
fn serialize_hotkeys<S: serde::Serializer>(map: &BTreeMap<String, String>, s: S) -> Result<S::Ok, S::Error> {
    let changed: BTreeMap<&String, &String> = map
        .iter()
        .filter(|(id, text)| {
            let default = crate::hotkeys::Action::from_id(id).map(crate::hotkeys::Action::default_chord);
            default.is_none() || text.parse::<crate::hotkeys::Chord>().ok() != default
        })
        .collect();
    changed.serialize(s)
}

/// A boolean key that never makes the whole file unusable: a value of another type reads as absent.
fn lenient_opt_bool<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<bool>, D::Error> {
    Ok(toml::Value::deserialize(d)?.as_bool())
}

/// A boolean key that never makes the whole file unusable: a value of another type reads as `false`.
fn lenient_bool<'de, D: serde::Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    Ok(toml::Value::deserialize(d)?.as_bool().unwrap_or(false))
}

/// Like [`lenient_bool`] for a key that is on by default: a value of another type reads as `true`.
fn lenient_true<'de, D: serde::Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    Ok(toml::Value::deserialize(d)?.as_bool().unwrap_or(true))
}

/// A whole-number key that never makes the whole file unusable: a value of another type, or a negative one, reads as 0.
fn lenient_u32<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    Ok(toml::Value::deserialize(d)?
        .as_integer()
        .and_then(|i| u32::try_from(i).ok())
        .unwrap_or(0))
}

fn default_version() -> u32 {
    SCHEMA_VERSION
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiState {
    pub menu_x: f32,
    pub menu_y: f32,
    /// Size of the settings menu window in output pixels (T-52a).
    pub menu_w: f32,
    pub menu_h: f32,
    pub menu_opacity: f32,
    pub menu_collapsed: bool,
    /// `[global.ui] scale`: `"auto"` (the DPI of the output's monitor, the default) or a factor from 0.75 to 3.0.
    pub scale: UiScale,
    /// `[global.ui] first_run_done`: the first-run hint was dismissed with "Do not show again" (`true`), or is
    /// still due (`false`, written by a first run). Absent (a file from before the hint): never shown.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_opt_bool"
    )]
    pub first_run_done: Option<bool>,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            menu_x: 20.0,
            menu_y: 20.0,
            menu_w: 460.0,
            menu_h: 560.0,
            menu_opacity: 0.9,
            menu_collapsed: false,
            scale: UiScale::Auto,
            first_run_done: None,
        }
    }
}

impl UiState {
    /// Whether the first-run hint is shown: only when a first run asked for it and it was not dismissed for good.
    pub fn first_run_due(&self) -> bool {
        self.first_run_done == Some(false)
    }
}

/// Smallest and largest manual UI scale.
pub const UI_SCALE_RANGE: (f32, f32) = (0.75, 3.0);

/// The UI scale setting (`[global.ui] scale`): the monitor's DPI, or a fixed factor.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum UiScale {
    /// The DPI scale of the output's monitor (1 at 96 dpi), followed when it changes.
    #[default]
    Auto,
    /// A fixed factor within [`UI_SCALE_RANGE`].
    Manual(f32),
}

impl UiScale {
    /// A manual factor clamped to [`UI_SCALE_RANGE`] (a non-finite value gives `Auto`).
    pub fn manual(v: f32) -> UiScale {
        if v.is_finite() {
            UiScale::Manual(v.clamp(UI_SCALE_RANGE.0, UI_SCALE_RANGE.1))
        } else {
            UiScale::Auto
        }
    }

    /// The scale in effect on a monitor of DPI scale `monitor`.
    pub fn effective(self, monitor: f32) -> f32 {
        match self {
            UiScale::Auto if monitor.is_finite() && monitor > 0.0 => monitor,
            UiScale::Auto => 1.0,
            UiScale::Manual(v) => v.clamp(UI_SCALE_RANGE.0, UI_SCALE_RANGE.1),
        }
    }
}

impl Serialize for UiScale {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            UiScale::Auto => s.serialize_str("auto"),
            // Two decimals: 1.25 stays 1.25 in the file.
            UiScale::Manual(v) => s.serialize_f64((f64::from(*v) * 100.0).round() / 100.0),
        }
    }
}

impl<'de> Deserialize<'de> for UiScale {
    /// `"auto"` (any case) or a number (clamped); anything else reads as `auto`, so a bad value never makes the
    /// whole file unusable.
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = toml::Value::deserialize(d)?;
        Ok(match v {
            toml::Value::Float(f) => UiScale::manual(f as f32),
            toml::Value::Integer(i) => UiScale::manual(i as f32),
            _ => UiScale::Auto,
        })
    }
}

/// Output window state remembered between runs (T-57).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct OutputState {
    /// Outer rectangle of the framed windowed output in virtual-screen pixels: left, top, right, bottom. The
    /// position holds the monitor; a rectangle that is no longer on a monitor is ignored (`modes::restore_rect`).
    pub windowed: Option<[i32; 4]>,
    /// The output ignores the mouse while the menu is closed: clicks reach the captured window under it
    /// (`output::behaviour`). Off by default (PLAN D-8). Toggled by its hotkey (Ctrl+Alt+F6) or the menu
    /// (Interface, "Click-through").
    #[serde(deserialize_with = "lenient_bool")]
    pub click_through: bool,
    /// Frame rate cap of the output loop in frames per second, 0 = no cap (every display refresh). The loop
    /// skips whole display refreshes, so the rate in force is the display rate divided by a whole number (a cap of
    /// 60 on a 144 Hz display gives 48). Read through [`OutputState::frame_cap`] (clamped). Menu: Display and pacing.
    #[serde(deserialize_with = "lenient_u32")]
    pub frame_cap: u32,
    /// Present without waiting for the vertical blank (`IMMEDIATE`): lower latency, the picture may tear, and
    /// variable refresh rate displays can follow the frame rate. Off by default (FIFO). Falls back to FIFO when the
    /// surface does not offer it. Menu: Display and pacing.
    #[serde(deserialize_with = "lenient_bool")]
    pub tearing: bool,
}

/// The lowest and highest frame cap that is applied (a value in between is kept, outside it is clamped; 0 is off).
pub const FRAME_CAP_RANGE: (u32, u32) = (10, 1000);

impl OutputState {
    /// The frame cap in force: 0 (none), else the stored value clamped to [`FRAME_CAP_RANGE`].
    pub fn frame_cap(&self) -> u32 {
        if self.frame_cap == 0 {
            0
        } else {
            self.frame_cap.clamp(FRAME_CAP_RANGE.0, FRAME_CAP_RANGE.1)
        }
    }
}

/// The value of `gpu.adapter` that means the automatic choice.
pub const GPU_ADAPTER_AUTO: &str = "auto";

/// GPU choice (HW-3): the adapter that runs the whole chain (capture device and Vulkan renderer). Read at start
/// only; a change needs a restart. `--gpu` wins over it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GpuSettings {
    /// `auto` (the adapter owning the monitor the output covers), an adapter identity as the menu and
    /// `--list-adapters` write it (`NVIDIA RTX A2000 Laptop GPU (PCI 10DE:25B8:0A3D1028, LUID 00000000:00017801)`:
    /// name and PCI ids, the LUID only a hint within one boot, review D-02), or a case-insensitive part of the
    /// adapter name (`RTX`, `Intel`). An adapter LUID alone (`00000000:00017801`, earlier builds) is still read,
    /// but Windows keeps it only until a restart. Parsed by `adapters::AdapterSpec::parse`.
    pub adapter: String,
}

impl Default for GpuSettings {
    fn default() -> Self {
        Self {
            adapter: GPU_ADAPTER_AUTO.to_string(),
        }
    }
}

/// Capture settings (`[global.capture]`, A29).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CaptureSettings {
    /// How captured frames reach the renderer: `zero_copy` (the default: GPU sharing when the machine allows it,
    /// else the CPU copy, with an automatic fallback on any failure) or `cpu` (the simple CPU copy that works on
    /// every adapter pair). `auto` reads as the default. Any other value reads as the default with a note (the hard
    /// range of an enum, WP4). Stored only when it differs from the default. `--capture-path` wins over it.
    pub path: String,
    /// Lift the Windows limit on the capture rate (`GraphicsCaptureSession.MinUpdateInterval`, Windows 11 24H2 and
    /// later; the session otherwise delivers at most about 60 frames per second). On by default; ignored where the
    /// system does not offer it. Stored only when off. Menu: Display and pacing.
    #[serde(deserialize_with = "lenient_true")]
    pub max_rate: bool,
}

impl Default for CaptureSettings {
    fn default() -> Self {
        Self {
            path: CapturePath::default().id().to_string(),
            max_rate: true,
        }
    }
}

impl CaptureSettings {
    /// The path asked for (the default for a value that is not one).
    pub fn path(&self) -> CapturePath {
        CapturePath::parse(&self.path).unwrap_or_default()
    }

    /// The default path and capture rate: not written to the file.
    pub fn is_default(&self) -> bool {
        self.path() == CapturePath::default() && self.max_rate
    }
}

/// The window the user attached to last (source selection, A23): the source-profile key (executable and class)
/// and the title as a hint. Written on every attach; used at start when `ask_at_start` is off.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LastSource {
    /// Executable file name (`Game.exe`); empty when it could not be queried (then the title must match).
    pub exe: String,
    /// Window class.
    pub class: String,
    /// Window title when it was chosen (a tie-breaker between windows of the same program and class).
    pub title: String,
}

/// Default of `[global.source] wait_s`: seconds a lost source is waited for (reattach to the same program and
/// class) before the "Choose a window" screen comes back (`--wait-source` wins).
pub const DEFAULT_WAIT_S: f64 = crate::lifecycle::DEFAULT_WAIT_SOURCE_S;

/// Source selection (`[global.source]`, A23): the product has no command line; the window to capture is chosen in
/// the output window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SourceSettings {
    /// Show the window picker at every start (default). Off: attach at once to the last program and class when
    /// one of its windows is running, else show the picker.
    pub ask_at_start: bool,
    /// Seconds a lost source is waited for before the picker comes back; 0 waits forever.
    pub wait_s: f64,
    /// The last window chosen.
    pub last: Option<LastSource>,
    /// Picture-area detection: largest code value (brightest channel, 0 to 255) still counted as black.
    pub bars_tolerance: u8,
    /// Picture-area detection: thinner bars are ignored (px).
    pub bars_min: u32,
    /// Picture-area detection, "Largest crop": largest fraction of an axis the bars may remove (0.10 to 0.99,
    /// default 0.97). It replaces `bars_max_crop` (default 0.6, the guard that blocked a small game in a large
    /// window, 2026-11-20): that key and `detect_bars_at_attach` (the Auto picture area replaced it) are read and
    /// dropped, so an old file gets the new default.
    pub bars_largest_crop: f64,
}

impl Default for SourceSettings {
    fn default() -> Self {
        Self {
            ask_at_start: true,
            wait_s: DEFAULT_WAIT_S,
            last: None,
            bars_tolerance: crate::bars::DEFAULT_TOLERANCE,
            bars_min: crate::bars::DEFAULT_MIN_BAR,
            bars_largest_crop: crate::bars::DEFAULT_MAX_CROP,
        }
    }
}

impl SourceSettings {
    /// The picture-area detection thresholds, made valid (tolerance as stored, at least 1 px, a fraction within
    /// [`crate::bars::MAX_CROP_RANGE`]).
    pub fn bars_config(&self) -> crate::bars::Config {
        let (lo, hi) = crate::bars::MAX_CROP_RANGE;
        crate::bars::Config {
            tolerance: self.bars_tolerance,
            min_bar: self.bars_min.max(1),
            max_crop: if self.bars_largest_crop.is_finite() {
                self.bars_largest_crop.clamp(lo, hi)
            } else {
                crate::bars::DEFAULT_MAX_CROP
            },
            corner: crate::bars::DEFAULT_CORNER,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Global {
    /// Name of the preset the overrides apply to (empty: table defaults).
    pub preset: String,
    /// Parameter id to value, only for ids that differ from the defaults.
    pub overrides: BTreeMap<String, toml::Value>,
    /// Action id to chord text (`F11`, `Ctrl+Alt+End`). In memory every action has an entry (the missing ones
    /// are filled with their default at load); the file holds only the chords that differ from their default.
    #[serde(serialize_with = "serialize_hotkeys")]
    pub hotkeys: BTreeMap<String, String>,
    /// The generation of the default hotkeys the file was written with ([`HOTKEY_DEFAULTS`]; absent: 1).
    #[serde(default = "legacy_hotkey_defaults")]
    pub hotkey_defaults: u32,
    pub ui: UiState,
    /// Windowed output rectangle (T-57).
    pub output: OutputState,
    /// The adapter that runs the chain (`[global.gpu] adapter`, HW-3).
    pub gpu: GpuSettings,
    /// The capture path (`[global.capture] path`, A29): written only when it is not the default (zero-copy), so a
    /// file without it follows the default.
    #[serde(skip_serializing_if = "CaptureSettings::is_default")]
    pub capture: CaptureSettings,
    /// Source selection: ask at start, lost-source wait, last choice (`[global.source]`, A23).
    pub source: SourceSettings,
}

impl Default for Global {
    fn default() -> Self {
        Self {
            preset: String::new(),
            overrides: BTreeMap::new(),
            hotkeys: default_hotkeys(),
            hotkey_defaults: HOTKEY_DEFAULTS,
            ui: UiState::default(),
            output: OutputState::default(),
            gpu: GpuSettings::default(),
            capture: CaptureSettings::default(),
            source: SourceSettings::default(),
        }
    }
}

pub fn default_hotkeys() -> BTreeMap<String, String> {
    DEFAULT_HOTKEYS
        .iter()
        .map(|(a, c)| (a.to_string(), c.to_string()))
        .collect()
}

/// Per-source settings (SRC-2, T-96). Unset fields keep the value of the preset and the global overrides
/// (for the modes: automatic). The app applies the profile of the attached window (executable and class)
/// on top of the preset and the global overrides; command-line flags override the profile.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SourceProfile {
    /// Executable file name, matched case insensitively.
    pub exe: String,
    /// Window class, matched case insensitively.
    pub class: String,
    /// Crop rectangle in captured pixels of the client area: left, top, right, bottom (x0, y0, x1, y1, the
    /// right and bottom edges exclusive). Used when the crop mode is `manual`.
    pub crop: Option<[u32; 4]>,
    /// Integer divisor (horizontal, and vertical unless `divisor_v` is set). Used when the divisor mode is
    /// `manual`.
    pub divisor: Option<u32>,
    /// Vertical divisor when it differs from `divisor` (T-96).
    pub divisor_v: Option<u32>,
    /// Pixel aspect ratio (`source_par`).
    pub par: Option<f32>,
    /// `source_eotf` value name: `drive_voltage` or `srgb`.
    pub eotf: Option<String>,
    /// Pixel clock in Hz.
    pub pixel_clock_hz: Option<f64>,
    /// Video bandwidth in Hz.
    pub bandwidth_hz: Option<f64>,
    pub line_multiplier: Option<u32>,
    /// `source_crop_mode` value name: `auto`, `manual` or `none` (T-96).
    pub crop_mode: Option<String>,
    /// The Source section's picture area (`bars::AreaMode`): `auto` (detected and followed, also when unset),
    /// `manual` (the `crop` rectangle) or `client` (the whole client area). Crop mode `manual` always reads as
    /// manual.
    pub picture_area: Option<String>,
    /// The client size (width, height) the manual `crop` was set for: a different captured size shows a warning.
    pub crop_client: Option<[u32; 2]>,
    /// `source_divisor_mode` value name: `auto`, `manual` or `none` (T-96).
    pub divisor_mode: Option<String>,
    /// `source_scale_mode` value name: `tube`, `native`, `integer`, `fit` or `fit_free` (T-96).
    pub scale_mode: Option<String>,
    /// `source_scale`: host px per source row in the integer mode, 0 = automatic (T-96).
    pub scale: Option<u32>,
    /// `source_center` (T-96).
    pub center: Option<bool>,
    /// `source_surround_color`, output code values 0 to 1 (T-96).
    pub surround_color: Option<[f32; 3]>,
    /// `source_display_aspect`, 0 = from the PAR (T-96).
    pub display_aspect: Option<f32>,
    /// `raster_aspect` value name: `par` or `fill_tube` (T-96).
    pub raster_aspect: Option<String>,
    /// `source_rotation` value name: `off`, `counter_clockwise` or `clockwise` (TATE: a vertical game).
    pub rotation: Option<String>,
}

/// The parameters a [`SourceProfile`] can set ([`SourceProfile::apply_to`]).
pub const PROFILE_PARAM_IDS: &[&str] = &[
    "source_eotf",
    "source_crop_mode",
    "source_divisor_mode",
    "source_scale_mode",
    "raster_aspect",
    "source_par",
    "line_multiplier",
    "source_scale",
    "source_center",
    "source_surround_color",
    "source_display_aspect",
    "source_rotation",
];

/// Sets the enum parameter `id` to the value called `name` (its `values` entry, `-` read as `_`). False
/// when the id is not an enum or the name is not one of its values.
pub fn set_enum_by_name(params: &mut Params, id: &str, name: &str) -> bool {
    let name = name.trim().replace('-', "_");
    PARAMS_META
        .iter()
        .find(|m| m.id == id && m.ty == ParamType::Enum)
        .and_then(|m| m.values.iter().position(|v| v.eq_ignore_ascii_case(&name)))
        .is_some_and(|i| params.set(id, ParamValue::Enum(i as u32)))
}

impl SourceProfile {
    /// Applies the parameter fields of the profile onto `params` (T-96). Returns one warning per invalid
    /// value (skipped). The crop rectangle and the divisor are not parameters: [`SourceProfile::crop_rect`]
    /// and [`SourceProfile::divisor_pair`]. The pixel clock and the bandwidth are kept for the signal
    /// stages and not applied here.
    pub fn apply_to(&self, params: &mut Params) -> Vec<String> {
        let mut warnings = Vec::new();
        let mut warn = |ok: bool, field: &str| {
            if !ok {
                warnings.push(format!(
                    "source profile {} ({}): invalid {field} ignored",
                    self.exe, self.class
                ));
            }
        };
        let enums = [
            ("source_eotf", &self.eotf, "eotf"),
            ("source_crop_mode", &self.crop_mode, "crop_mode"),
            ("source_divisor_mode", &self.divisor_mode, "divisor_mode"),
            ("source_scale_mode", &self.scale_mode, "scale_mode"),
            ("raster_aspect", &self.raster_aspect, "raster_aspect"),
            ("source_rotation", &self.rotation, "rotation"),
        ];
        for (id, value, field) in enums {
            if let Some(v) = value {
                warn(set_enum_by_name(params, id, v), field);
            }
        }
        if let Some(v) = self.par {
            warn(v > 0.0 && params.set("source_par", ParamValue::F32(v)), "par");
        }
        if let Some(v) = self.line_multiplier {
            warn(
                i32::try_from(v)
                    .is_ok_and(|v| (1..=2).contains(&v) && params.set("line_multiplier", ParamValue::I32(v))),
                "line_multiplier",
            );
        }
        if let Some(v) = self.scale {
            warn(
                v <= 10 && params.set("source_scale", ParamValue::I32(v as i32)),
                "scale",
            );
        }
        if let Some(v) = self.center {
            warn(params.set("source_center", ParamValue::Bool(v)), "center");
        }
        if let Some(c) = self.surround_color {
            warn(
                c.iter().all(|v| (0.0..=1.0).contains(v)) && params.set("source_surround_color", ParamValue::Vec3(c)),
                "surround_color",
            );
        }
        if let Some(v) = self.display_aspect {
            warn(
                v >= 0.0 && v.is_finite() && params.set("source_display_aspect", ParamValue::F32(v)),
                "display_aspect",
            );
        }
        warnings
    }

    /// Switches the parameters from the profile of one source to the profile of another (attach to another
    /// window, A23): every parameter a profile can set ([`PROFILE_PARAM_IDS`]) goes back to `base` (the preset in
    /// force with the global overrides, without any profile), then `self` applies on top. The other parameters
    /// (the tube, the user's edits of the session) are untouched. Returns the warnings of [`Self::apply_to`].
    pub fn switch_to(&self, params: &mut Params, base: &Params) -> Vec<String> {
        for id in PROFILE_PARAM_IDS {
            if let Some(v) = base.get(id) {
                params.set(id, v);
            }
        }
        self.apply_to(params)
    }

    /// The manual crop rectangle (`crop`), `None` when unset or empty.
    pub fn crop_rect(&self) -> Option<crate::scaling::Rect> {
        let [x0, y0, x1, y1] = self.crop?;
        (x1 > x0 && y1 > y0).then(|| crate::scaling::Rect {
            x: x0,
            y: y0,
            w: x1 - x0,
            h: y1 - y0,
        })
    }

    /// The manual divisor (`divisor`, `divisor_v`), `None` when unset or zero.
    pub fn divisor_pair(&self) -> Option<(u32, u32)> {
        let h = self.divisor.filter(|&d| d >= 1)?;
        let v = self.divisor_v.unwrap_or(h);
        (v >= 1).then_some((h, v))
    }
}

/// The settings file. Unknown tables and keys are ignored at load and dropped at the next write (the `[host]`
/// table of earlier builds, which held an unused panel size, included).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    #[serde(default = "default_version")]
    pub version: u32,
    pub global: Global,
    #[serde(rename = "source")]
    pub sources: Vec<SourceProfile>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: SCHEMA_VERSION,
            global: Global::default(),
            sources: Vec::new(),
        }
    }
}

impl Settings {
    /// Profile of a source, by executable file name and window class (case insensitive).
    pub fn find_source(&self, exe: &str, class: &str) -> Option<&SourceProfile> {
        self.sources
            .iter()
            .find(|p| p.exe.eq_ignore_ascii_case(exe) && p.class.eq_ignore_ascii_case(class))
    }

    /// Removes the profile of `exe` and `class` (case insensitive). Returns whether one was removed.
    pub fn remove_source(&mut self, exe: &str, class: &str) -> bool {
        let n = self.sources.len();
        self.sources
            .retain(|p| !(p.exe.eq_ignore_ascii_case(exe) && p.class.eq_ignore_ascii_case(class)));
        self.sources.len() != n
    }

    /// "Reset everything to defaults" (menu, owner definition 2026-11-20): the parameter overrides cleared (the
    /// preset in force stays, with its own values), the hotkeys, the UI scale, the GPU adapter, the capture path
    /// and click-through back to their defaults. Kept: the per-game profiles, the source choice and its preferences, the window rectangle, the menu
    /// layout, the first-run state; user presets and burn-in maps are files this never touches.
    pub fn reset_everything(&mut self) {
        let g = &mut self.global;
        g.overrides.clear();
        g.hotkeys = default_hotkeys();
        g.ui.scale = UiScale::Auto;
        g.gpu = GpuSettings::default();
        g.capture = CaptureSettings::default();
        g.output.click_through = false;
        g.output.frame_cap = 0;
        g.output.tearing = false;
    }

    /// Inserts or replaces the profile with the same executable and class.
    pub fn upsert_source(&mut self, profile: SourceProfile) {
        match self
            .sources
            .iter_mut()
            .find(|p| p.exe.eq_ignore_ascii_case(&profile.exe) && p.class.eq_ignore_ascii_case(&profile.class))
        {
            Some(slot) => *slot = profile,
            None => self.sources.push(profile),
        }
    }

    pub fn to_toml(&self) -> Result<String, toml::ser::Error> {
        toml::to_string_pretty(self)
    }

    /// Parses a file. Missing hotkeys are filled with their defaults; a remembered window rectangle that cannot
    /// be one (`modes::IRect::from_saved`) is dropped; the old default hotkeys of an earlier build's file are
    /// upgraded ([`Settings::from_toml_noted`]).
    pub fn from_toml(text: &str) -> Result<Self, String> {
        Self::from_toml_noted(text).map(|(s, _)| s)
    }

    /// [`Settings::from_toml`], with the notes for the user: what was upgraded (review D-01). A file of hotkey
    /// generation 1 ([`HOTKEY_DEFAULTS`]) wrote every chord, so a chord equal to an old default
    /// (`hotkeys::Action::old_default_text`) is one the user never changed: it becomes the new default. A chord the
    /// user changed stays, and so does an old default whose new default another action already uses.
    pub fn from_toml_noted(text: &str) -> Result<(Self, Vec<String>), String> {
        if let Some(v) = newer_version(text) {
            return Err(format!(
                "schema version {v} is newer than the supported {SCHEMA_VERSION}"
            ));
        }
        let mut s: Settings = toml::from_str(text).map_err(|e| e.to_string())?;
        s.version = SCHEMA_VERSION;
        let mut notes = Vec::new();
        if s.global.hotkey_defaults < HOTKEY_DEFAULTS {
            notes.extend(upgrade_hotkeys(&mut s.global.hotkeys));
            s.global.hotkey_defaults = HOTKEY_DEFAULTS;
        }
        for (a, c) in DEFAULT_HOTKEYS {
            s.global.hotkeys.entry(a.to_string()).or_insert_with(|| c.to_string());
        }
        let w = &mut s.global.output.windowed;
        *w = w.filter(|a| crate::modes::IRect::from_saved(*a).is_some());
        // WP4: an enum value outside its range reads as the default, with a note; the file is normalised.
        let path = &mut s.global.capture.path;
        match CapturePath::parse(path) {
            Some(p) => *path = p.id().to_string(),
            None => {
                notes.push(format!(
                    "[global.capture] path = \"{path}\" is not zero_copy or cpu: the default (zero-copy) is used"
                ));
                *path = CapturePath::default().id().to_string();
            }
        }
        Ok((s, notes))
    }
}

/// Review D-01: the old default chords (Ctrl+Alt+Q, O, B, which fire on AltGr) of a generation-1 file become the
/// new defaults. Returns the note for the user (none when nothing changed).
fn upgrade_hotkeys(hotkeys: &mut BTreeMap<String, String>) -> Option<String> {
    use crate::hotkeys::{Action, Chord};
    let parsed = |t: &str| t.parse::<Chord>().ok();
    let mut done = Vec::new();
    let mut kept = Vec::new();
    for a in Action::ALL {
        let Some(old) = a.old_default_text() else { continue };
        let Some(text) = hotkeys.get(a.id()) else { continue };
        if parsed(text) != parsed(old) {
            continue;
        }
        let new = a.default_chord();
        let user = hotkeys
            .iter()
            .find(|(id, t)| id.as_str() != a.id() && parsed(t) == Some(new))
            .map(|(id, _)| id.clone());
        match user {
            Some(other) => kept.push(format!("{} stays {old} ({new} is used by {other})", a.describe())),
            None => {
                hotkeys.insert(a.id().to_string(), new.to_string());
                done.push(format!("{} {new} (was {old})", a.describe()));
            }
        }
    }
    if done.is_empty() && kept.is_empty() {
        return None;
    }
    let mut note = String::from(
        "Hotkeys: the old Ctrl+Alt+letter defaults also fired on AltGr (they swallowed characters such as '@' on \
         many keyboard layouts).",
    );
    if !done.is_empty() {
        note.push_str(&format!(" New defaults: {}.", done.join(", ")));
    }
    if !kept.is_empty() {
        note.push_str(&format!(" Not changed: {}.", kept.join(", ")));
    }
    Some(note)
}

pub(crate) fn to_value(v: ParamValue) -> toml::Value {
    use toml::Value as V;
    let arr = |a: &[f32]| V::Array(a.iter().map(|x| V::Float(*x as f64)).collect());
    match v {
        ParamValue::F32(x) => V::Float(x as f64),
        ParamValue::I32(x) => V::Integer(x as i64),
        ParamValue::Bool(x) => V::Boolean(x),
        ParamValue::Enum(x) => V::Integer(x as i64),
        ParamValue::Vec2(a) => arr(&a),
        ParamValue::Vec3(a) => arr(&a),
        ParamValue::Vec4(a) => arr(&a),
    }
}

fn floats<const N: usize>(v: &toml::Value) -> Option<[f32; N]> {
    let a = v.as_array()?;
    if a.len() != N {
        return None;
    }
    let mut out = [0.0; N];
    for (o, x) in out.iter_mut().zip(a) {
        *o = x.as_float().or_else(|| x.as_integer().map(|i| i as f64))? as f32;
    }
    Some(out)
}

pub(crate) fn from_value(ty: ParamType, v: &toml::Value) -> Option<ParamValue> {
    Some(match ty {
        ParamType::F32 => ParamValue::F32(v.as_float().or_else(|| v.as_integer().map(|i| i as f64))? as f32),
        ParamType::I32 => ParamValue::I32(i32::try_from(v.as_integer()?).ok()?),
        ParamType::Bool => ParamValue::Bool(v.as_bool()?),
        ParamType::Enum => ParamValue::Enum(u32::try_from(v.as_integer()?).ok()?),
        ParamType::Vec2 => ParamValue::Vec2(floats(v)?),
        ParamType::Vec3 => ParamValue::Vec3(floats(v)?),
        ParamType::Vec4 => ParamValue::Vec4(floats(v)?),
    })
}

/// The ids whose value differs from the parameter table default, with their values.
pub fn overrides_from_params(params: &Params) -> BTreeMap<String, toml::Value> {
    let mut out = BTreeMap::new();
    for m in PARAMS_META {
        if let Some(v) = params.get(m.id) {
            if v != m.default {
                out.insert(m.id.to_string(), to_value(v));
            }
        }
    }
    out
}

/// Records a settings menu edit in the overrides (T-53): for every id whose value differs between `before` and
/// `after` (what the user touched), the override becomes the new value, or is removed when the new value equals
/// `reference` (the preset in force with the source profile and the command-line options, which the next start
/// rebuilds without an override). Ids the user did not touch keep their override state, so values that come
/// from the source profile or the command line are never written as global overrides. Returns whether the
/// overrides changed.
pub fn record_edit(
    overrides: &mut BTreeMap<String, toml::Value>,
    before: &Params,
    after: &Params,
    reference: &Params,
) -> bool {
    let mut changed = false;
    for m in PARAMS_META {
        let (Some(b), Some(a)) = (before.get(m.id), after.get(m.id)) else {
            continue;
        };
        if a == b {
            continue;
        }
        if reference.get(m.id) == Some(a) {
            changed |= overrides.remove(m.id).is_some();
        } else {
            let v = to_value(a);
            if overrides.get(m.id) != Some(&v) {
                overrides.insert(m.id.to_string(), v);
                changed = true;
            }
        }
    }
    changed
}

/// "Reset look overrides" (menu): every parameter override goes, except the quality ones (`quality_tier`,
/// `quality_auto`: they describe the machine, not the look). Returns whether something was removed.
pub fn reset_look_overrides(overrides: &mut BTreeMap<String, toml::Value>) -> bool {
    let n = overrides.len();
    overrides.retain(|id, _| crate::quality::PARAM_IDS.contains(&id.as_str()));
    overrides.len() != n
}

/// Keeps a copy of `dir\settings.toml` before a reset ([`BACKUP_FILE`], or the first free `settings.toml.bak.<n>`
/// when an older different copy is there). `None` when there is no file or it cannot be copied.
pub fn backup_current(dir: &Path) -> Option<PathBuf> {
    let path = dir.join(SETTINGS_FILE);
    path.exists().then(|| backup(dir, &path)).flatten()
}

/// Applies the overrides of `settings.toml` onto `params` through [`crate::presets::apply_overrides_checked`]:
/// an unknown id, a malformed or non-finite value is skipped, a value outside the hard range is clamped. Returns
/// one warning per problem, each naming `settings.toml`.
pub fn apply_overrides(params: &mut Params, overrides: &BTreeMap<String, toml::Value>) -> Vec<String> {
    crate::presets::apply_overrides_checked(params, overrides, SETTINGS_FILE)
}

/// `%APPDATA%\Phoxium`, or `None` when `APPDATA` is not set.
pub fn default_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("Phoxium"))
}

/// The `version` of a settings text when it is newer than [`SCHEMA_VERSION`] (the rest of the file may not
/// parse with this build's types), `None` otherwise or when the text is not TOML.
fn newer_version(text: &str) -> Option<i64> {
    let table: toml::Table = text.parse().ok()?;
    let v = table.get("version")?.as_integer()?;
    (v > i64::from(SCHEMA_VERSION)).then_some(v)
}

/// What [`load`] found in the settings file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileState {
    /// No file: defaults, written at the first change.
    Missing,
    /// A valid file.
    Valid,
    /// Not valid TOML or not the expected types: defaults; the file is kept as `.bak` and the session's
    /// settings replace it.
    Invalid,
    /// It exists but could not be read (locked, no permission): defaults; the session never writes it.
    Unreadable,
    /// A newer schema (written by a newer build): defaults; kept as `.bak` and never written by the session.
    Newer,
}

/// Result of [`load`].
#[derive(Debug)]
pub struct Loaded {
    pub settings: Settings,
    /// Set when the file could not be used (defaults are returned); says what happens to the file.
    pub warning: Option<String>,
    pub state: FileState,
    /// Whether the session may write `settings.toml` (false for an unreadable file or a newer schema: the
    /// session runs with the defaults in memory and leaves the file as it is).
    pub writable: bool,
    /// What the load upgraded, for the user (the old default hotkeys, review D-01); the app writes the file back
    /// at once so that the note is shown once.
    pub notes: Vec<String>,
}

impl Loaded {
    /// A first start (the first-run card is due): no settings file at all. A corrupt, unreadable or newer file is
    /// not a first start: its warning is shown instead (on the window picker) and the card stays away.
    pub fn first_start(&self) -> bool {
        self.state == FileState::Missing
    }
}

/// Loads `dir\settings.toml`. A missing file gives defaults without a warning; see [`FileState`] for the rest.
pub fn load(dir: &Path) -> Loaded {
    let path = dir.join(SETTINGS_FILE);
    let unusable = |state: FileState, reason: String| {
        // An unreadable file cannot be copied either: nothing to keep, nothing will overwrite it.
        let kept = if state == FileState::Unreadable {
            None
        } else {
            backup(dir, &path)
        };
        let fate = if state == FileState::Invalid {
            "it will be replaced by this session's settings"
        } else {
            "this session will not write the settings file (changes last until exit)"
        };
        let warning = format!(
            "{} cannot be used ({reason}); defaults are used and {fate}{}",
            path.display(),
            kept.map(|b| format!("; a copy is kept as {}", b.display()))
                .unwrap_or_default()
        );
        Loaded {
            settings: Settings::default(),
            warning: Some(warning),
            state,
            writable: state == FileState::Invalid,
            notes: Vec::new(),
        }
    };
    let text = match fs::read(&path) {
        Ok(bytes) => String::from_utf8(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Loaded {
                settings: Settings::default(),
                warning: None,
                state: FileState::Missing,
                writable: true,
                notes: Vec::new(),
            }
        }
        Err(e) => return unusable(FileState::Unreadable, e.to_string()),
    };
    let text = match text {
        Ok(t) => t,
        Err(_) => return unusable(FileState::Invalid, "not UTF-8 text".to_string()),
    };
    if newer_version(&text).is_some() {
        return unusable(FileState::Newer, Settings::from_toml(&text).unwrap_err());
    }
    match Settings::from_toml_noted(&text) {
        Ok((settings, notes)) => Loaded {
            settings,
            warning: None,
            state: FileState::Valid,
            writable: true,
            notes,
        },
        Err(e) => unusable(FileState::Invalid, e),
    }
}

/// Keeps a copy of the unusable `path` as [`BACKUP_FILE`]. The first copy is never overwritten: the same content
/// again is already kept; other content goes to the first free `settings.toml.bak.<n>`. Returns the copy's path.
fn backup(dir: &Path, path: &Path) -> Option<PathBuf> {
    let content = fs::read(path).ok()?;
    for n in 0..100u32 {
        let bak = if n == 0 {
            dir.join(BACKUP_FILE)
        } else {
            dir.join(format!("{BACKUP_FILE}.{n}"))
        };
        match fs::read(&bak) {
            Ok(existing) if existing == content => return Some(bak),
            Ok(_) => continue,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // `create_new`: never replaces a file that appeared in between.
                let mut f = fs::OpenOptions::new().write(true).create_new(true).open(&bak).ok()?;
                return f.write_all(&content).is_ok().then_some(bak);
            }
            Err(_) => continue,
        }
    }
    None
}

/// Writes `dir\settings.toml` atomically (a temporary file in the same directory named per process, flushed,
/// then renamed). Blocking: the live app goes through [`Writer`].
pub fn save(dir: &Path, settings: &Settings) -> crate::Res<()> {
    fs::create_dir_all(dir)?;
    let text = settings.to_toml()?;
    let tmp = dir.join(format!("{SETTINGS_FILE}.{}.tmp", std::process::id()));
    let written = (|| -> std::io::Result<()> {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()
    })();
    if let Err(e) = written.and_then(|()| fs::rename(&tmp, dir.join(SETTINGS_FILE))) {
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

/// The settings writer of the live app (R-31): [`save`] on a background thread, so no file I/O (and no `fsync`)
/// happens on the render thread. Latest wins: a request replaces one not yet started. [`Writer::finish`] (or the
/// drop) writes what is pending and joins the thread. A failed write is a warning on stderr, never an error.
pub struct Writer {
    shared: std::sync::Arc<WriterShared>,
    thread: Option<std::thread::JoinHandle<()>>,
    dir: PathBuf,
}

struct WriterShared {
    state: std::sync::Mutex<WriterState>,
    cv: std::sync::Condvar,
}

#[derive(Default)]
struct WriterState {
    pending: Option<Box<Settings>>,
    busy: bool,
    stop: bool,
    /// The error of the last write, until [`Writer::wait_idle`] or [`Writer::finish`] takes it.
    error: Option<String>,
    /// The outcome of the latest write: its error, `None` once a write succeeded ([`Writer::problem`]).
    failing: Option<String>,
}

impl WriterShared {
    fn lock(&self) -> std::sync::MutexGuard<'_, WriterState> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Writer {
    /// A writer for `dir\settings.toml`. Without a thread (the spawn failed), every save writes at once.
    pub fn new(dir: PathBuf) -> Writer {
        let shared = std::sync::Arc::new(WriterShared {
            state: std::sync::Mutex::new(WriterState::default()),
            cv: std::sync::Condvar::new(),
        });
        let (s, d) = (shared.clone(), dir.clone());
        let thread = std::thread::Builder::new()
            .name("settings-writer".into())
            .spawn(move || writer_loop(&s, &d))
            .ok();
        Writer { shared, thread, dir }
    }

    /// Asks for `settings` to be written; returns at once (replaces a request not started yet).
    pub fn save(&self, settings: &Settings) {
        if self.thread.is_none() {
            let result = save(&self.dir, settings);
            let mut st = self.shared.lock();
            st.failing = result.as_ref().err().map(ToString::to_string);
            if let Err(e) = result {
                eprintln!("warning: cannot save the settings: {e}");
                st.error = Some(e.to_string());
            }
            return;
        }
        self.shared.lock().pending = Some(Box::new(settings.clone()));
        self.shared.cv.notify_all();
    }

    /// Waits until every request is written; returns the error of the last failed write, if any.
    pub fn wait_idle(&self) -> Option<String> {
        let mut st = self.shared.lock();
        while self.thread.is_some() && (st.pending.is_some() || st.busy) {
            st = self
                .shared
                .cv
                .wait(st)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        st.error.take()
    }

    /// The error of the latest finished write while it stands (a disk full, a read-only folder, a file locked by
    /// another program), `None` once a later write succeeded. Never waits: the app polls it for a notice.
    pub fn problem(&self) -> Option<String> {
        self.shared.lock().failing.clone()
    }

    /// Writes what is pending and stops the thread (at exit). Returns the last write error, if any.
    pub fn finish(mut self) -> Option<String> {
        self.stop()
    }

    fn stop(&mut self) -> Option<String> {
        self.shared.lock().stop = true;
        self.shared.cv.notify_all();
        if let Some(t) = self.thread.take() {
            if t.join().is_err() {
                return Some("the settings writer thread panicked".into());
            }
        }
        self.shared.lock().error.take()
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn writer_loop(shared: &WriterShared, dir: &Path) {
    let mut st = shared.lock();
    loop {
        if let Some(s) = st.pending.take() {
            st.busy = true;
            drop(st);
            let result = save(dir, &s);
            st = shared.lock();
            st.busy = false;
            st.failing = result.as_ref().err().map(ToString::to_string);
            if let Err(e) = result {
                eprintln!("warning: cannot save the settings: {e}");
                st.error = Some(e.to_string());
            }
            shared.cv.notify_all();
        } else if st.stop {
            return;
        } else {
            st = shared.cv.wait(st).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("phoxium-settings-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn some_id() -> &'static crate::params::ParamMeta {
        PARAMS_META
            .iter()
            .find(|m| m.ty == ParamType::F32)
            .expect("an f32 parameter")
    }

    #[test]
    fn round_trip() {
        let dir = temp_dir("rt");
        let mut s = Settings::default();
        s.global.preset = "sony-trinitron".into();
        s.global.ui.menu_opacity = 0.5;
        s.global.hotkeys.insert("toggle_menu".into(), "Ctrl+F12".into());
        s.upsert_source(SourceProfile {
            exe: "Game.exe".into(),
            class: "UnityWndClass".into(),
            crop: Some([1, 2, 3, 4]),
            divisor: Some(4),
            par: Some(1.25),
            eotf: Some("srgb".into()),
            pixel_clock_hz: Some(5.37e6),
            bandwidth_hz: Some(3.0e6),
            line_multiplier: Some(2),
            divisor_v: Some(3),
            crop_mode: Some("manual".into()),
            divisor_mode: Some("manual".into()),
            scale_mode: Some("integer".into()),
            scale: Some(3),
            center: Some(false),
            surround_color: Some([0.25, 0.5, 0.0]),
            display_aspect: Some(4.0 / 3.0),
            raster_aspect: Some("par".into()),
            rotation: Some("clockwise".into()),
            picture_area: Some("manual".into()),
            crop_client: Some([640, 480]),
        });
        let mut p = Params::default();
        let m = some_id();
        let ParamValue::F32(d) = m.default else { unreachable!() };
        assert!(p.set(m.id, ParamValue::F32(d + 0.25)));
        s.global.overrides = overrides_from_params(&p);
        save(&dir, &s).unwrap();
        let names: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, [SETTINGS_FILE], "no temporary file left");
        let back = load(&dir);
        assert!(back.warning.is_none());
        assert_eq!((back.state, back.writable), (FileState::Valid, true));
        assert_eq!(back.settings, s);
        let mut q = Params::default();
        assert!(apply_overrides(&mut q, &back.settings.global.overrides).is_empty());
        assert_eq!(q.get(m.id), p.get(m.id));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_gives_defaults_silently() {
        let dir = temp_dir("missing");
        let l = load(&dir);
        assert!(l.warning.is_none());
        assert_eq!(l.settings, Settings::default());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_file_gives_defaults_warning_and_bak() {
        let dir = temp_dir("corrupt");
        fs::write(dir.join(SETTINGS_FILE), "this is = = not toml [[").unwrap();
        let l = load(&dir);
        assert_eq!(l.settings, Settings::default());
        assert!(l.warning.is_some());
        assert_eq!((l.state, l.writable), (FileState::Invalid, true));
        assert_eq!(
            fs::read_to_string(dir.join(BACKUP_FILE)).unwrap(),
            "this is = = not toml [["
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// R-15: the first `.bak` is never overwritten. Loading the same invalid file twice (the app loads it more
    /// than once at start) keeps one copy; another invalid file goes to the next free name.
    #[test]
    fn two_invalid_loads_keep_the_first_backup() {
        let dir = temp_dir("bak");
        fs::write(dir.join(SETTINGS_FILE), "first = = [[").unwrap();
        assert!(load(&dir).warning.unwrap().contains(BACKUP_FILE));
        assert!(load(&dir).warning.is_some());
        assert_eq!(fs::read_to_string(dir.join(BACKUP_FILE)).unwrap(), "first = = [[");
        assert!(!dir.join(format!("{BACKUP_FILE}.1")).exists(), "same content: one copy");
        fs::write(dir.join(SETTINGS_FILE), "second = = [[").unwrap();
        let l = load(&dir);
        assert!(l.warning.unwrap().contains(&format!("{BACKUP_FILE}.1")));
        assert_eq!(fs::read_to_string(dir.join(BACKUP_FILE)).unwrap(), "first = = [[");
        assert_eq!(
            fs::read_to_string(dir.join(format!("{BACKUP_FILE}.1"))).unwrap(),
            "second = = [["
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// R-15: a newer schema gives defaults, a copy, and a session that never writes the file.
    #[test]
    fn newer_schema_is_set_aside() {
        let dir = temp_dir("newer");
        // A newer build may use types this one does not know: still recognised as newer, not as invalid.
        let text = format!(
            "version = {}\n[global]\npreset = 7\n[global.ui]\nmenu_x = \"far\"\n",
            SCHEMA_VERSION + 1
        );
        fs::write(dir.join(SETTINGS_FILE), &text).unwrap();
        let l = load(&dir);
        assert_eq!((l.state, l.writable), (FileState::Newer, false));
        assert!(
            l.warning.as_ref().unwrap().contains("will not write"),
            "{:?}",
            l.warning
        );
        assert_eq!(fs::read_to_string(dir.join(BACKUP_FILE)).unwrap(), text);
        assert_eq!(fs::read_to_string(dir.join(SETTINGS_FILE)).unwrap(), text);
        let _ = fs::remove_dir_all(&dir);
    }

    /// R-15: a file that exists but cannot be read (opened exclusively by another program) gives defaults and a
    /// session that never writes it.
    #[test]
    fn an_unreadable_file_is_never_overwritten() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = temp_dir("locked");
        let path = dir.join(SETTINGS_FILE);
        fs::write(&path, "version = 1\n[global]\npreset = \"mine\"\n").unwrap();
        {
            // Share mode 0: nobody else may read, write or delete it while the handle is open.
            let _lock = fs::OpenOptions::new().read(true).share_mode(0).open(&path).unwrap();
            let l = load(&dir);
            assert_eq!((l.state, l.writable), (FileState::Unreadable, false));
            assert!(l.warning.unwrap().contains("will not write"));
            assert!(!dir.join(BACKUP_FILE).exists());
        }
        assert_eq!(load(&dir).settings.global.preset, "mine", "untouched");
        let _ = fs::remove_dir_all(&dir);
    }

    /// R-31: the writer thread keeps the latest request; after `finish` the file is complete.
    #[test]
    fn the_writer_keeps_the_last_of_quick_saves() {
        let dir = temp_dir("writer");
        let w = Writer::new(dir.clone());
        let mut s = Settings::default();
        for preset in ["a", "b", "c"] {
            s.global.preset = preset.into();
            w.save(&s);
        }
        assert_eq!(w.wait_idle(), None);
        assert_eq!(load(&dir).settings.global.preset, "c");
        s.global.preset = "d".into();
        w.save(&s);
        assert_eq!(w.finish(), None);
        let l = load(&dir);
        assert_eq!((l.state, l.settings.global.preset.as_str()), (FileState::Valid, "d"));
        // A write that fails (the folder is a file) is reported, not a panic.
        let blocked = dir.join("blocked");
        fs::write(&blocked, "x").unwrap();
        let w = Writer::new(blocked);
        w.save(&s);
        assert!(w.wait_idle().is_some());
        let _ = fs::remove_dir_all(&dir);
    }

    /// Write failures of the background writer (a full disk or a permission error give the same paths: the
    /// temporary file cannot be made, or the rename onto settings.toml fails): each is reported (`problem` while it
    /// stands, the error once to `wait_idle`), leaves no temporary file and never breaks the file already there;
    /// the writer keeps working and a later write that can succeed clears the problem.
    #[test]
    fn the_writer_survives_write_failures_and_recovers() {
        let dir = temp_dir("writer-fail");
        let w = Writer::new(dir.clone());
        let mut s = Settings::default();
        s.global.preset = "first".into();
        w.save(&s);
        assert_eq!(w.wait_idle(), None);
        assert_eq!(w.problem(), None);
        // The rename fails: settings.toml is replaced by a folder (a locked or read-only target behaves alike).
        fs::remove_file(dir.join(SETTINGS_FILE)).unwrap();
        fs::create_dir(dir.join(SETTINGS_FILE)).unwrap();
        s.global.preset = "second".into();
        w.save(&s);
        assert!(w.wait_idle().is_some(), "reported once");
        assert!(w.problem().is_some(), "and standing until a write succeeds");
        assert_eq!(w.wait_idle(), None, "the error was taken");
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
        // The cause goes: the next write succeeds and the problem clears.
        fs::remove_dir(dir.join(SETTINGS_FILE)).unwrap();
        s.global.preset = "third".into();
        w.save(&s);
        assert_eq!(w.wait_idle(), None);
        assert_eq!(w.problem(), None);
        assert_eq!(load(&dir).settings.global.preset, "third");
        assert_eq!(w.finish(), None);
        // The folder cannot be made at all (its parent is a file): every write fails, nothing panics, the writer
        // still stops cleanly.
        let parent = dir.join("file");
        fs::write(&parent, "x").unwrap();
        let w = Writer::new(parent.join("sub"));
        for _ in 0..3 {
            w.save(&s);
        }
        assert!(w.wait_idle().is_some());
        assert!(w.problem().is_some());
        assert!(w.finish().is_none(), "the error was already taken");
        let _ = fs::remove_dir_all(&dir);
    }

    /// The first-run card and a damaged settings file: only a missing file is a first start. A corrupt, unreadable
    /// or newer file shows its warning instead; a file whose first-run or click-through key has a wrong type stays
    /// valid (those keys are read leniently), so one bad value never costs the user the other settings.
    #[test]
    fn a_damaged_settings_file_is_not_a_first_start_and_bad_flags_are_lenient() {
        let dir = temp_dir("first-run");
        let l = load(&dir);
        assert!(l.first_start(), "no file: first start");
        fs::write(dir.join(SETTINGS_FILE), "version = 1\n[global\npreset = ").unwrap();
        let l = load(&dir);
        assert_eq!(l.state, FileState::Invalid);
        assert!(!l.first_start() && l.warning.is_some());
        assert!(!l.settings.global.ui.first_run_due(), "no card over the warning");
        fs::write(
            dir.join(SETTINGS_FILE),
            "version = 1\n[global]\npreset = \"kept\"\n[global.ui]\nfirst_run_done = \"yes\"\n[global.output]\n\
             click_through = 3\n",
        )
        .unwrap();
        let l = load(&dir);
        assert_eq!(l.state, FileState::Valid, "{:?}", l.warning);
        assert_eq!(l.settings.global.preset, "kept");
        assert_eq!(l.settings.global.ui.first_run_done, None);
        assert!(!l.settings.global.output.click_through);
        assert!(!l.first_start());
        // A newer schema is not a first start either.
        fs::write(dir.join(SETTINGS_FILE), format!("version = {}\n", SCHEMA_VERSION + 1)).unwrap();
        let l = load(&dir);
        assert!(l.state == FileState::Newer && !l.first_start());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn overrides_hold_only_changed_ids() {
        assert!(overrides_from_params(&Params::default()).is_empty());
        let mut p = Params::default();
        let m = some_id();
        let ParamValue::F32(d) = m.default else { unreachable!() };
        p.set(m.id, ParamValue::F32(d + 1.0));
        let o = overrides_from_params(&p);
        assert_eq!(o.len(), 1);
        assert!(o.contains_key(m.id));
        // Setting it back to the default removes it.
        p.set(m.id, m.default);
        assert!(overrides_from_params(&p).is_empty());
        // The saved file mentions no unchanged id.
        let mut s = Settings::default();
        s.global.overrides = BTreeMap::new();
        let text = s.to_toml().unwrap();
        for meta in PARAMS_META {
            assert!(!text.contains(meta.id), "{} leaked into the file", meta.id);
        }
    }

    #[test]
    fn menu_edits_record_only_touched_ids_against_the_reference() {
        let preset = Params {
            mask_pitch_mm: 0.5,
            ..Params::default()
        };
        // The reference: the preset with a source profile value on top.
        let mut reference = preset;
        reference.source_par = 1.25;
        let mut o = BTreeMap::new();
        let before = reference;
        let mut after = before;
        after.exposure = 2.0;
        assert!(record_edit(&mut o, &before, &after, &reference));
        assert_eq!(
            o.keys().collect::<Vec<_>>(),
            ["exposure"],
            "the profile value is not written"
        );
        // The next start: the preset, the overrides, then the profile, gives the edited parameters back.
        let mut next = preset;
        assert!(apply_overrides(&mut next, &o).is_empty());
        next.source_par = 1.25;
        assert_eq!(next, after);
        // Back to the reference value: the override goes.
        let mut back = after;
        back.exposure = reference.exposure;
        assert!(record_edit(&mut o, &after, &back, &reference));
        assert!(o.is_empty());
        // Nothing touched: nothing recorded.
        assert!(!record_edit(&mut o, &back, &back, &reference));
        // A touched value equal to the preset but not to the reference (the profile's id) is kept as written.
        let mut p = back;
        p.source_par = 1.0;
        assert!(record_edit(&mut o, &back, &p, &reference));
        assert_eq!(o.get("source_par"), Some(&toml::Value::Float(1.0)));
    }

    #[test]
    fn bad_overrides_warn_and_are_skipped() {
        let mut o = BTreeMap::new();
        o.insert("no_such_param".to_string(), toml::Value::Float(1.0));
        o.insert(some_id().id.to_string(), toml::Value::String("x".into()));
        let mut p = Params::default();
        let w = apply_overrides(&mut p, &o);
        assert_eq!(w.len(), 2);
        // WP4: the warnings name the file; a value outside the hard range is clamped with a warning too.
        assert!(w.iter().all(|w| w.starts_with("settings.toml: ")), "{w:?}");
        let mut o = BTreeMap::new();
        o.insert("tube_visible_height_mm".to_string(), toml::Value::Float(-1.0));
        let w = apply_overrides(&mut p, &o);
        assert!(w.len() == 1 && w[0].starts_with("settings.toml: "), "{w:?}");
        assert!(p.tube_visible_height_mm > 0.0);
    }

    #[test]
    fn every_parameter_type_round_trips_through_overrides() {
        for m in PARAMS_META {
            let v = to_value(m.default);
            assert_eq!(from_value(m.ty, &v), Some(m.default), "{}", m.id);
        }
    }

    #[test]
    fn hotkey_defaults_fill_gaps_and_the_menu_key_is_f10() {
        let s = Settings::from_toml("version = 1\n[global.hotkeys]\nquit = \"Q\"\n").unwrap();
        assert_eq!(s.global.hotkeys["toggle_fullscreen"], "F11");
        assert_eq!(s.global.hotkeys["toggle_menu"], "F10");
        assert_eq!(s.global.hotkeys["toggle_bypass"], "Ctrl+Alt+F7");
        assert_eq!(s.global.hotkeys["previous_preset"], "Ctrl+Alt+PageUp");
        // Parsing keeps the text as written; the validation is `hotkeys::Bindings::from_settings`.
        assert_eq!(s.global.hotkeys["quit"], "Q");
        let (b, w) = crate::hotkeys::Bindings::from_settings(&s.global.hotkeys);
        assert_eq!(b.get(crate::hotkeys::Action::Quit).unwrap().to_string(), "Ctrl+Alt+End");
        assert_eq!(w.len(), 1, "{w:?}");
    }

    /// The text an earlier build wrote: every chord, the old defaults, no `hotkey_defaults`.
    const OLD_FILE: &str =
        "version = 1\n[global]\npreset = \"\"\n[global.hotkeys]\nnext_preset = \"Ctrl+Alt+PageDown\"\n\
         previous_preset = \"Ctrl+Alt+PageUp\"\nquit = \"Ctrl+Alt+Q\"\ntoggle_bypass = \"Ctrl+Alt+B\"\n\
         toggle_fullscreen = \"F11\"\ntoggle_menu = \"F10\"\ntoggle_overlay = \"Ctrl+Alt+O\"\n";

    /// Review D-01: an earlier build's file with the old defaults gets the new ones, with one note; written back,
    /// it stores no chord and the next load upgrades nothing.
    #[test]
    fn old_default_hotkeys_are_upgraded_once_and_defaults_are_not_stored() {
        let (s, notes) = Settings::from_toml_noted(OLD_FILE).unwrap();
        assert_eq!(s.global.hotkeys, default_hotkeys());
        assert_eq!(s.global.hotkey_defaults, HOTKEY_DEFAULTS);
        assert_eq!(notes.len(), 1, "{notes:?}");
        let n = &notes[0];
        assert!(n.contains("AltGr"), "{n}");
        assert!(n.contains("quit Ctrl+Alt+End (was Ctrl+Alt+Q)"), "{n}");
        assert!(n.contains("overlay Ctrl+Alt+F9 (was Ctrl+Alt+O)"), "{n}");
        assert!(n.contains("A/B bypass Ctrl+Alt+F7 (was Ctrl+Alt+B)"), "{n}");
        let text = s.to_toml().unwrap();
        assert!(!text.contains("Ctrl+Alt"), "no default chord is stored:\n{text}");
        assert!(text.contains("hotkey_defaults = 2"), "{text}");
        let (back, notes) = Settings::from_toml_noted(&text).unwrap();
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(back.global.hotkeys, default_hotkeys());
        // A chord picked by the user after the upgrade, even an old default, stays as it is.
        let mut s = back;
        s.global.hotkeys.insert("quit".into(), "Ctrl+Alt+Q".into());
        let text = s.to_toml().unwrap();
        assert!(text.contains("quit = \"Ctrl+Alt+Q\""), "{text}");
        let (back, notes) = Settings::from_toml_noted(&text).unwrap();
        assert!(notes.is_empty());
        assert_eq!(back.global.hotkeys["quit"], "Ctrl+Alt+Q");
        // A fresh file has nothing to upgrade.
        let (_, notes) = Settings::from_toml_noted("version = 1\n").unwrap();
        assert!(notes.is_empty());
    }

    /// Review D-01: in an earlier build's file, a chord the user changed stays, and an old default whose new
    /// default another action already uses stays too (with the reason in the note).
    #[test]
    fn customised_hotkeys_survive_the_upgrade() {
        let text = OLD_FILE
            .replace("toggle_overlay = \"Ctrl+Alt+O\"", "toggle_overlay = \"Ctrl+Alt+M\"")
            .replace("toggle_menu = \"F10\"", "toggle_menu = \"Ctrl+Alt+F7\"");
        let (s, notes) = Settings::from_toml_noted(&text).unwrap();
        assert_eq!(s.global.hotkeys["toggle_overlay"], "Ctrl+Alt+M");
        assert_eq!(s.global.hotkeys["toggle_menu"], "Ctrl+Alt+F7");
        assert_eq!(
            s.global.hotkeys["toggle_bypass"], "Ctrl+Alt+B",
            "its new default is the menu's"
        );
        assert_eq!(s.global.hotkeys["quit"], "Ctrl+Alt+End");
        let n = &notes[0];
        assert!(n.contains("quit Ctrl+Alt+End (was Ctrl+Alt+Q)"), "{n}");
        assert!(
            n.contains("A/B bypass stays Ctrl+Alt+B (Ctrl+Alt+F7 is used by toggle_menu)"),
            "{n}"
        );
        assert!(!n.contains("overlay"), "{n}");
        // Nothing old in the file: no note.
        let text = OLD_FILE
            .replace("\"Ctrl+Alt+Q\"", "\"Ctrl+Alt+End\"")
            .replace("\"Ctrl+Alt+O\"", "\"F9\"")
            .replace("\"Ctrl+Alt+B\"", "\"F8\"");
        let (_, notes) = Settings::from_toml_noted(&text).unwrap();
        assert!(notes.is_empty(), "{notes:?}");
    }

    #[test]
    fn output_state_round_trips_and_defaults_to_nothing() {
        assert_eq!(Settings::default().global.output.windowed, None);
        let mut s = Settings::default();
        s.global.output.windowed = Some([100, -50, 1380, 770]);
        let back = Settings::from_toml(&s.to_toml().unwrap()).unwrap();
        assert_eq!(back.global.output.windowed, Some([100, -50, 1380, 770]));
        // A file without the table keeps working.
        let s = Settings::from_toml("version = 1\n").unwrap();
        assert_eq!(s.global.output, OutputState::default());
    }

    #[test]
    fn capture_path_defaults_to_zero_copy_is_stored_only_when_cpu_and_clamps_the_rest() {
        // The default is zero-copy, and it is not written.
        let s = Settings::default();
        assert_eq!(s.global.capture.path(), CapturePath::ZeroCopy);
        let text = s.to_toml().unwrap();
        assert!(!text.contains("[global.capture]"), "{text}");
        assert_eq!(
            Settings::from_toml(&text).unwrap().global.capture.path(),
            CapturePath::ZeroCopy
        );
        // An explicit cpu is written and read back as cpu.
        let mut s = Settings::default();
        s.global.capture.path = CapturePath::Cpu.id().to_string();
        let text = s.to_toml().unwrap();
        assert!(text.contains("[global.capture]\npath = \"cpu\""), "{text}");
        assert_eq!(
            Settings::from_toml(&text).unwrap().global.capture.path(),
            CapturePath::Cpu
        );
        // A file of v0.2.0 has no such key: the new default.
        assert_eq!(
            Settings::from_toml("version = 1\n[global.gpu]\nadapter = \"auto\"\n")
                .unwrap()
                .global
                .capture
                .path(),
            CapturePath::ZeroCopy
        );
        // Other spellings, normalised; an explicit zero_copy or auto is the default, so not written back.
        for v in ["zero-copy", "zero_copy", "auto"] {
            let (s, notes) =
                Settings::from_toml_noted(&format!("version = 1\n[global.capture]\npath = \"{v}\"\n")).unwrap();
            assert_eq!(s.global.capture.path, "zero_copy", "{v}");
            assert!(notes.is_empty());
            assert!(!s.to_toml().unwrap().contains("[global.capture]"));
        }
        let (s, _) = Settings::from_toml_noted("version = 1\n[global.capture]\npath = \"CPU\"\n").unwrap();
        assert_eq!(s.global.capture.path, "cpu");
        // Outside the range: the default, with a note.
        let (s, notes) = Settings::from_toml_noted("version = 1\n[global.capture]\npath = \"gpu\"\n").unwrap();
        assert_eq!(s.global.capture.path, "zero_copy");
        assert!(notes.iter().any(|n| n.contains("[global.capture] path")), "{notes:?}");
        // Reset everything puts it back to the default.
        let mut s = Settings::from_toml("version = 1\n[global.capture]\npath = \"cpu\"\n").unwrap();
        s.reset_everything();
        assert_eq!(s.global.capture.path(), CapturePath::ZeroCopy);
        assert!(!s.to_toml().unwrap().contains("[global.capture]"));
    }

    #[test]
    fn gpu_adapter_defaults_to_auto_and_round_trips() {
        assert_eq!(Settings::default().global.gpu.adapter, "auto");
        // A file without the table keeps working and means auto.
        let s = Settings::from_toml("version = 1\n").unwrap();
        assert_eq!(s.global.gpu.adapter, GPU_ADAPTER_AUTO);
        let s = Settings::from_toml("version = 1\n[global.gpu]\nadapter = \"00000000:00017801\"\n").unwrap();
        assert_eq!(s.global.gpu.adapter, "00000000:00017801");
        let back = Settings::from_toml(&s.to_toml().unwrap()).unwrap();
        assert_eq!(back.global.gpu.adapter, "00000000:00017801");
    }

    #[test]
    fn profile_lookup_by_exe_and_class() {
        let mut s = Settings::default();
        let mk = |exe: &str, class: &str, d| SourceProfile {
            exe: exe.into(),
            class: class.into(),
            divisor: Some(d),
            ..Default::default()
        };
        s.upsert_source(mk("game.exe", "A", 2));
        s.upsert_source(mk("game.exe", "B", 3));
        s.upsert_source(mk("other.exe", "A", 4));
        assert_eq!(s.find_source("GAME.EXE", "a").unwrap().divisor, Some(2));
        assert_eq!(s.find_source("game.exe", "B").unwrap().divisor, Some(3));
        assert!(s.find_source("game.exe", "C").is_none());
        s.upsert_source(mk("Game.exe", "A", 9));
        assert_eq!(s.sources.len(), 3);
        assert_eq!(s.find_source("game.exe", "A").unwrap().divisor, Some(9));
    }

    /// T-96: a source profile sets the source parameters by name, rejects invalid values with a warning, and
    /// gives the manual crop rectangle and divisor. An empty profile changes nothing.
    #[test]
    fn source_profile_applies_to_the_parameters() {
        use crate::params::{
            RASTER_ASPECT_FILL_TUBE, SOURCE_CROP_MODE_NONE, SOURCE_DIVISOR_MODE_MANUAL, SOURCE_EOTF_SRGB,
            SOURCE_ROTATION_COUNTER_CLOCKWISE, SOURCE_SCALE_MODE_FIT_FREE, SOURCE_SCALE_MODE_INTEGER,
        };
        let mut p = Params::default();
        assert!(SourceProfile::default().apply_to(&mut p).is_empty());
        assert_eq!(p, Params::default());
        let profile = SourceProfile {
            exe: "game.exe".into(),
            class: "C".into(),
            crop: Some([8, 4, 328, 244]),
            divisor: Some(2),
            divisor_v: Some(3),
            par: Some(8.0 / 7.0),
            eotf: Some("srgb".into()),
            line_multiplier: Some(2),
            crop_mode: Some("none".into()),
            divisor_mode: Some("manual".into()),
            scale_mode: Some("integer".into()),
            scale: Some(4),
            center: Some(false),
            surround_color: Some([0.1, 0.2, 0.3]),
            display_aspect: Some(1.5),
            raster_aspect: Some("fill_tube".into()),
            rotation: Some("counter-clockwise".into()),
            ..Default::default()
        };
        assert!(profile.apply_to(&mut p).is_empty());
        assert_eq!(p.source_rotation, SOURCE_ROTATION_COUNTER_CLOCKWISE);
        assert_eq!(p.source_eotf, SOURCE_EOTF_SRGB);
        assert_eq!(p.source_crop_mode, SOURCE_CROP_MODE_NONE);
        assert_eq!(p.source_divisor_mode, SOURCE_DIVISOR_MODE_MANUAL);
        assert_eq!(p.source_scale_mode, SOURCE_SCALE_MODE_INTEGER);
        assert_eq!(p.raster_aspect, RASTER_ASPECT_FILL_TUBE);
        assert_eq!((p.source_scale, p.source_center, p.line_multiplier), (4, false, 2));
        assert_eq!(p.source_par, 8.0 / 7.0);
        assert_eq!(p.source_surround_color, [0.1, 0.2, 0.3]);
        assert_eq!(p.source_display_aspect, 1.5);
        assert_eq!(
            profile.crop_rect(),
            Some(crate::scaling::Rect {
                x: 8,
                y: 4,
                w: 320,
                h: 240
            })
        );
        assert_eq!(profile.divisor_pair(), Some((2, 3)));
        // The command-line spelling `fit-free` is accepted.
        assert!(set_enum_by_name(&mut p, "source_scale_mode", "fit-free"));
        assert_eq!(p.source_scale_mode, SOURCE_SCALE_MODE_FIT_FREE);
        let bad = SourceProfile {
            scale_mode: Some("huge".into()),
            scale: Some(11),
            line_multiplier: Some(3),
            surround_color: Some([2.0, 0.0, 0.0]),
            crop: Some([10, 10, 5, 20]),
            divisor: Some(0),
            ..Default::default()
        };
        let before = p;
        assert_eq!(bad.apply_to(&mut p).len(), 4);
        assert_eq!(p, before, "invalid values change nothing");
        assert_eq!((bad.crop_rect(), bad.divisor_pair()), (None, None));
    }

    /// A settings file written before T-96 (no new fields) still loads, the new fields unset.
    #[test]
    fn profiles_without_the_t96_fields_still_load() {
        let s = Settings::from_toml("version = 1\n[[source]]\nexe = \"a.exe\"\nclass = \"B\"\ndivisor = 3\n").unwrap();
        let p = s.find_source("a.exe", "b").unwrap();
        assert_eq!(p.divisor_pair(), Some((3, 3)));
        assert_eq!((p.scale_mode.as_deref(), p.crop_mode.as_deref()), (None, None));
    }

    /// A23: `[global.source]` defaults to asking at start with a 30 s wait and no last choice, round-trips, and
    /// a file without the table keeps working.
    #[test]
    fn source_selection_settings_round_trip() {
        let d = Settings::default().global.source;
        assert!(d.ask_at_start);
        assert_eq!(d.wait_s, 30.0);
        assert_eq!(d.last, None);
        let s = Settings::from_toml("version = 1\n").unwrap();
        assert_eq!(s.global.source, SourceSettings::default());
        let mut s = Settings::default();
        s.global.source = SourceSettings {
            ask_at_start: false,
            wait_s: 12.5,
            last: Some(LastSource {
                exe: "Mina.exe".into(),
                class: "UnityWndClass".into(),
                title: "Mina the Hollower".into(),
            }),
            bars_tolerance: 12,
            bars_min: 3,
            bars_largest_crop: 0.5,
        };
        let text = s.to_toml().unwrap();
        assert!(text.contains("[global.source]"), "{text}");
        let back = Settings::from_toml(&text).unwrap();
        assert_eq!(back.global.source, s.global.source);
        // A partial table fills the rest with the defaults.
        let s = Settings::from_toml("version = 1\n[global.source]\nask_at_start = false\n").unwrap();
        assert!(!s.global.source.ask_at_start);
        assert_eq!(s.global.source.wait_s, DEFAULT_WAIT_S);
        // Picture-area detection: the documented thresholds, invalid values made valid.
        let d = &s.global.source;
        assert_eq!(d.bars_config(), crate::bars::Config::default());
        assert_eq!(d.bars_config().max_crop, 0.97);
        let odd = SourceSettings {
            bars_min: 0,
            bars_largest_crop: 3.0,
            ..SourceSettings::default()
        };
        assert_eq!((odd.bars_config().min_bar, odd.bars_config().max_crop), (1, 0.99));
    }

    /// The first black-bar design's keys (2026-11-20): `bars_max_crop` (its 0.6 default blocked a small game in a
    /// large window) and `detect_bars_at_attach` (replaced by the Auto picture area) are dropped at load, so an
    /// existing file gets the new 97 % default and the next write no longer has them.
    #[test]
    fn old_black_bar_keys_are_migrated() {
        for old in ["0.6", "0.5", "0.95"] {
            let text = format!(
                "version = 1\n[global.source]\ndetect_bars_at_attach = true\nbars_tolerance = 10\nbars_max_crop = \
                 {old}\n"
            );
            let s = Settings::from_toml(&text).unwrap();
            let c = s.global.source.bars_config();
            assert_eq!((c.tolerance, c.max_crop), (10, crate::bars::DEFAULT_MAX_CROP), "{old}");
            let out = s.to_toml().unwrap();
            assert!(
                !out.contains("bars_max_crop") && !out.contains("detect_bars_at_attach"),
                "{out}"
            );
            assert!(out.contains("bars_largest_crop = 0.97"), "{out}");
        }
        // A profile of the first design (crop mode auto after a reset, or a detected manual crop) still loads:
        // auto reads as the Auto picture area, manual as Manual.
        let s = Settings::from_toml(
            "version = 1\n[[source]]\nexe = \"a.exe\"\nclass = \"B\"\ncrop_mode = \"auto\"\n[[source]]\nexe = \
             \"c.exe\"\nclass = \"D\"\ncrop_mode = \"manual\"\ncrop = [8, 4, 328, 244]\n",
        )
        .unwrap();
        for (exe, class, mode) in [
            ("a.exe", "B", crate::bars::AreaMode::Auto),
            ("c.exe", "D", crate::bars::AreaMode::Manual),
        ] {
            let p = s.find_source(exe, class).unwrap();
            let mut params = Params::default();
            assert!(p.apply_to(&mut params).is_empty());
            assert_eq!(crate::bars::AreaMode::of(&params, p), mode, "{exe}");
        }
    }

    /// A23: attaching to another program swaps the profile values and keeps everything else.
    #[test]
    fn switching_profiles_resets_the_old_profile_values_only() {
        use crate::params::SOURCE_SCALE_MODE_INTEGER;
        let base = Params {
            mask_pitch_mm: 0.4,
            ..Params::default()
        };
        let a = SourceProfile {
            par: Some(8.0 / 7.0),
            scale_mode: Some("integer".into()),
            ..Default::default()
        };
        let b = SourceProfile {
            center: Some(false),
            ..Default::default()
        };
        let mut p = base;
        assert!(a.switch_to(&mut p, &base).is_empty());
        assert_eq!(
            (p.source_par, p.source_scale_mode),
            (8.0 / 7.0, SOURCE_SCALE_MODE_INTEGER)
        );
        p.exposure = 1.7; // an edit of the session
        assert!(b.switch_to(&mut p, &base).is_empty());
        assert_eq!(p.source_par, base.source_par, "the old profile's PAR is gone");
        assert_eq!(p.source_scale_mode, base.source_scale_mode);
        assert!(!p.source_center);
        assert_eq!((p.exposure, p.mask_pitch_mm), (1.7, 0.4), "the rest is untouched");
        // Every id of the list is a real parameter.
        for id in PROFILE_PARAM_IDS {
            assert!(Params::default().get(id).is_some(), "{id}");
        }
    }

    /// R-41: the `[host]` table of earlier builds (an unused panel size) loads without a warning and is not
    /// written back.
    #[test]
    fn an_old_host_table_is_dropped() {
        let dir = temp_dir("host");
        fs::write(
            dir.join(SETTINGS_FILE),
            "version = 1\n[global]\npreset = \"p\"\n[host]\nmonitor = \"\"\npanel_mm_override = [530.0, 300.0]\n",
        )
        .unwrap();
        let l = load(&dir);
        assert_eq!((l.state, l.warning), (FileState::Valid, None));
        assert_eq!(l.settings.global.preset, "p");
        save(&dir, &l.settings).unwrap();
        let text = fs::read_to_string(dir.join(SETTINGS_FILE)).unwrap();
        assert!(!text.contains("host") && !text.contains("panel_mm"), "{text}");
        assert!(!Settings::default().to_toml().unwrap().contains("host"));
        let _ = fs::remove_dir_all(&dir);
    }

    /// R-42: an absurd remembered window rectangle is dropped at load (the default placement is used).
    #[test]
    fn an_absurd_window_rectangle_is_dropped_at_load() {
        let s = Settings::from_toml("version = 1\n[global.output]\nwindowed = [-2147483648, 0, 2147483647, 100]\n")
            .unwrap();
        assert_eq!(s.global.output.windowed, None);
        let s = Settings::from_toml("version = 1\n[global.output]\nwindowed = [10, 20, 1290, 740]\n").unwrap();
        assert_eq!(s.global.output.windowed, Some([10, 20, 1290, 740]));
    }

    /// Frame rate cap, tearing and the capture rate limit: defaults (no cap, vsync, limit lifted), absent in files of
    /// earlier builds, round-trip, and a wrong type or value never makes the file unusable.
    #[test]
    fn frame_cap_tearing_and_max_rate_defaults_round_trip_and_read_leniently() {
        let d = Settings::default();
        assert_eq!(d.global.output.frame_cap(), 0);
        assert!(!d.global.output.tearing);
        assert!(d.global.capture.max_rate && d.global.capture.is_default());
        let old = "version = 1
[global.output]
windowed = [10, 20, 1290, 740]
";
        let o = Settings::from_toml(old).unwrap();
        assert_eq!(
            (
                o.global.output.frame_cap(),
                o.global.output.tearing,
                o.global.capture.max_rate
            ),
            (0, false, true)
        );
        let mut s = Settings::default();
        s.global.output.frame_cap = 60;
        s.global.output.tearing = true;
        s.global.capture.max_rate = false;
        let text = s.to_toml().unwrap();
        assert!(
            text.contains("frame_cap = 60") && text.contains("tearing = true"),
            "{text}"
        );
        assert!(text.contains("max_rate = false"), "{text}");
        let back = Settings::from_toml(&text).unwrap();
        assert_eq!((back.global.output.frame_cap(), back.global.output.tearing), (60, true));
        assert!(!back.global.capture.max_rate);
        // Wrong types read as the defaults; the cap is clamped; a negative cap is none.
        let bad = "version = 1
[global.output]
frame_cap = \"fast\"
tearing = 3
[global.capture]
max_rate = \"yes\"
";
        let b = Settings::from_toml(bad).unwrap();
        assert_eq!(
            (
                b.global.output.frame_cap(),
                b.global.output.tearing,
                b.global.capture.max_rate
            ),
            (0, false, true)
        );
        let low = Settings::from_toml(
            "version = 1
[global.output]
frame_cap = 3
",
        )
        .unwrap();
        assert_eq!(low.global.output.frame_cap(), FRAME_CAP_RANGE.0);
        let high = Settings::from_toml(
            "version = 1
[global.output]
frame_cap = 99999
",
        )
        .unwrap();
        assert_eq!(high.global.output.frame_cap(), FRAME_CAP_RANGE.1);
        let neg = Settings::from_toml(
            "version = 1
[global.output]
frame_cap = -5
",
        )
        .unwrap();
        assert_eq!(neg.global.output.frame_cap(), 0);
    }

    /// `[global.output] click_through`: off by default and in a file of an earlier build (no key), round-trips,
    /// and a file with the new hotkey missing still binds Ctrl+Alt+F6.
    #[test]
    fn click_through_defaults_off_and_round_trips() {
        assert!(!Settings::default().global.output.click_through);
        let old = "version = 1\n[global.output]\nwindowed = [10, 20, 1290, 740]\n";
        assert!(!Settings::from_toml(old).unwrap().global.output.click_through);
        let mut s = Settings::default();
        s.global.output.click_through = true;
        let text = s.to_toml().unwrap();
        assert!(text.contains("click_through = true"), "{text}");
        assert!(Settings::from_toml(&text).unwrap().global.output.click_through);
        let mut hk = default_hotkeys();
        hk.remove("toggle_click_through");
        let (b, w) = crate::hotkeys::Bindings::from_settings(&hk);
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(
            b.get(crate::hotkeys::Action::ToggleClickThrough)
                .map(|c| c.to_string())
                .as_deref(),
            Some("Ctrl+Alt+F6")
        );
    }

    /// The menu's UI scale and first-run keys (`[global.ui] scale`, `first_run_done`) round-trip; a file without
    /// them loads with `auto` and no first-run hint; a bad scale reads as `auto` and never makes the file invalid.
    #[test]
    fn ui_scale_and_first_run_round_trip_and_old_files_still_load() {
        let d = Settings::default().global.ui;
        assert_eq!(
            (d.scale, d.first_run_done, d.first_run_due()),
            (UiScale::Auto, None, false)
        );
        let text = Settings::default().to_toml().unwrap();
        assert!(
            text.contains("scale = \"auto\"") && !text.contains("first_run_done"),
            "{text}"
        );
        let mut s = Settings::default();
        s.global.ui.scale = UiScale::manual(1.25);
        s.global.ui.first_run_done = Some(false);
        let text = s.to_toml().unwrap();
        assert!(
            text.contains("scale = 1.25") && text.contains("first_run_done = false"),
            "{text}"
        );
        let back = Settings::from_toml(&text).unwrap();
        assert_eq!(back.global.ui, s.global.ui);
        assert!(back.global.ui.first_run_due());
        s.global.ui.first_run_done = Some(true);
        let back = Settings::from_toml(&s.to_toml().unwrap()).unwrap();
        assert!(!back.global.ui.first_run_due());
        // A file of the previous build: no scale, no first-run key.
        let old =
            "version = 1\n[global.ui]\nmenu_x = 30.0\nmenu_y = 40.0\nmenu_w = 500.0\nmenu_h = 600.0\nmenu_opacity \
                   = 0.8\nmenu_collapsed = false\n";
        let s = Settings::from_toml(old).unwrap();
        assert_eq!((s.global.ui.menu_x, s.global.ui.scale), (30.0, UiScale::Auto));
        assert!(
            !s.global.ui.first_run_due(),
            "an existing file never shows the first-run hint"
        );
        // Odd values: clamped, or `auto`.
        for (v, want) in [
            ("9.0", UiScale::Manual(3.0)),
            ("0.1", UiScale::Manual(0.75)),
            ("2", UiScale::Manual(2.0)),
            ("\"AUTO\"", UiScale::Auto),
            ("\"huge\"", UiScale::Auto),
            ("[1, 2]", UiScale::Auto),
        ] {
            let s = Settings::from_toml(&format!("version = 1\n[global.ui]\nscale = {v}\n")).unwrap();
            assert_eq!(s.global.ui.scale, want, "{v}");
        }
        assert_eq!(UiScale::manual(f32::NAN), UiScale::Auto);
        assert_eq!(UiScale::Auto.effective(1.5), 1.5);
        assert_eq!(UiScale::Auto.effective(0.0), 1.0);
        assert_eq!(UiScale::Manual(2.0).effective(1.0), 2.0);
    }

    /// "Reset everything" clears the overrides, the hotkeys, the UI scale and the adapter, and keeps the per-game
    /// profiles, the preset, the source choice and the window; "Reset look overrides" keeps the quality ones.
    #[test]
    fn resets_clear_what_they_say_and_keep_the_rest() {
        let mut s = Settings::default();
        s.global.preset = "pvm-20m4u".into();
        s.global.overrides.insert("exposure".into(), toml::Value::Float(1.5));
        s.global
            .overrides
            .insert("quality_tier".into(), toml::Value::Integer(2));
        s.global.hotkeys.insert("toggle_menu".into(), "Ctrl+Alt+M".into());
        s.global.ui.scale = UiScale::manual(2.0);
        s.global.ui.menu_opacity = 0.5;
        s.global.gpu.adapter = "Intel".into();
        s.global.output.windowed = Some([10, 20, 1290, 740]);
        s.global.source.ask_at_start = false;
        s.upsert_source(SourceProfile {
            exe: "game.exe".into(),
            class: "C".into(),
            divisor: Some(3),
            ..SourceProfile::default()
        });
        let mut look = s.global.overrides.clone();
        assert!(reset_look_overrides(&mut look));
        assert_eq!(look.keys().collect::<Vec<_>>(), ["quality_tier"]);
        assert!(!reset_look_overrides(&mut look), "nothing left to remove");
        let mut r = s.clone();
        r.reset_everything();
        assert!(r.global.overrides.is_empty());
        assert_eq!(r.global.hotkeys, default_hotkeys());
        assert_eq!(r.global.ui.scale, UiScale::Auto);
        assert_eq!(r.global.gpu.adapter, GPU_ADAPTER_AUTO);
        assert_eq!(r.global.preset, "pvm-20m4u");
        assert_eq!(r.sources, s.sources);
        assert_eq!(r.global.source, s.global.source);
        assert_eq!(r.global.output, s.global.output);
        assert_eq!(r.global.ui.menu_opacity, 0.5);
        // Profiles: removal by program and class, case insensitive.
        assert!(!r.remove_source("game.exe", "other"));
        assert!(r.remove_source("GAME.EXE", "c"));
        assert!(r.sources.is_empty());
    }

    /// The reset writes a copy of the file first; no file, no copy.
    #[test]
    fn a_reset_keeps_a_backup_of_the_file() {
        let dir = temp_dir("resetbak");
        assert_eq!(backup_current(&dir), None);
        let mut s = Settings::default();
        s.global.preset = "mine".into();
        save(&dir, &s).unwrap();
        let bak = backup_current(&dir).unwrap();
        assert_eq!(bak, dir.join(BACKUP_FILE));
        assert_eq!(
            fs::read_to_string(&bak).unwrap(),
            fs::read_to_string(dir.join(SETTINGS_FILE)).unwrap()
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
