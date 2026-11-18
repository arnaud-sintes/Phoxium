//! Persistent settings (T-51, UX-2, PRF-4, SRC-2): `%APPDATA%\ultimateCRT\settings.toml`.
//!
//! Holds the global settings (preset name, parameter overrides, hotkey chords, UI state), the host profile
//! (monitor and panel size) and the source profiles keyed by executable file name and window class.
//! The overrides hold only the ids that differ from the parameter defaults ([`overrides_from_params`]); the
//! diff against a preset is computed by the settings menu (T-53). Writes are atomic (temporary file, then
//! rename). A corrupt or too new file gives defaults plus a warning and is kept as `settings.toml.bak`.

use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::params::{ParamType, ParamValue, Params, PARAMS_META};

/// Schema version written to the file. Files with a higher version are not understood and are set aside.
pub const SCHEMA_VERSION: u32 = 1;
pub const SETTINGS_FILE: &str = "settings.toml";
pub const BACKUP_FILE: &str = "settings.toml.bak";

/// Hotkey action ids with their default chords. F11 and F12 are reserved for fullscreen and the menu.
pub const DEFAULT_HOTKEYS: &[(&str, &str)] = &[
    ("toggle_fullscreen", "F11"),
    ("toggle_menu", "F12"),
    ("quit", "Ctrl+Alt+Q"),
];

fn default_version() -> u32 {
    SCHEMA_VERSION
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiState {
    pub menu_x: f32,
    pub menu_y: f32,
    pub menu_opacity: f32,
    pub menu_collapsed: bool,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            menu_x: 20.0,
            menu_y: 20.0,
            menu_opacity: 0.9,
            menu_collapsed: false,
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
    /// Action id to chord text (`F11`, `Ctrl+Alt+Q`).
    pub hotkeys: BTreeMap<String, String>,
    pub ui: UiState,
}

impl Default for Global {
    fn default() -> Self {
        Self {
            preset: String::new(),
            overrides: BTreeMap::new(),
            hotkeys: default_hotkeys(),
            ui: UiState::default(),
        }
    }
}

pub fn default_hotkeys() -> BTreeMap<String, String> {
    DEFAULT_HOTKEYS
        .iter()
        .map(|(a, c)| (a.to_string(), c.to_string()))
        .collect()
}

/// Output monitor and physical panel size (UX-2). The size comes from EDID unless overridden.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HostProfile {
    /// Monitor device name, empty for the primary one.
    pub monitor: String,
    /// Panel width and height in mm, from EDID.
    pub panel_mm: Option<[f32; 2]>,
    /// Manual override of the panel size in mm; wins over `panel_mm`.
    pub panel_mm_override: Option<[f32; 2]>,
}

impl Default for HostProfile {
    fn default() -> Self {
        Self {
            monitor: String::new(),
            panel_mm: None,
            // Dell P2418D, 53 x 30 cm (PLAN T-51).
            panel_mm_override: Some([530.0, 300.0]),
        }
    }
}

impl HostProfile {
    /// The panel size to use: the manual override, else EDID.
    pub fn effective_panel_mm(&self) -> Option<[f32; 2]> {
        self.panel_mm_override.or(self.panel_mm)
    }
}

/// Per-source settings (SRC-2). Unset fields mean automatic.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SourceProfile {
    /// Executable file name, matched case insensitively.
    pub exe: String,
    /// Window class, matched case insensitively.
    pub class: String,
    /// Crop in pixels: left, top, right, bottom.
    pub crop: Option<[u32; 4]>,
    pub divisor: Option<u32>,
    /// Pixel aspect ratio.
    pub par: Option<f32>,
    pub eotf: Option<String>,
    /// Pixel clock in Hz.
    pub pixel_clock_hz: Option<f64>,
    /// Video bandwidth in Hz.
    pub bandwidth_hz: Option<f64>,
    pub line_multiplier: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    #[serde(default = "default_version")]
    pub version: u32,
    pub global: Global,
    pub host: HostProfile,
    #[serde(rename = "source")]
    pub sources: Vec<SourceProfile>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: SCHEMA_VERSION,
            global: Global::default(),
            host: HostProfile::default(),
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

    /// Parses a file. Missing hotkeys are filled with their defaults.
    pub fn from_toml(text: &str) -> Result<Self, String> {
        let mut s: Settings = toml::from_str(text).map_err(|e| e.to_string())?;
        if s.version > SCHEMA_VERSION {
            return Err(format!(
                "schema version {} is newer than the supported {SCHEMA_VERSION}",
                s.version
            ));
        }
        s.version = SCHEMA_VERSION;
        for (a, c) in DEFAULT_HOTKEYS {
            s.global.hotkeys.entry(a.to_string()).or_insert_with(|| c.to_string());
        }
        Ok(s)
    }
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

/// Applies overrides onto `params`. Returns one warning per unknown id or malformed value (skipped).
pub fn apply_overrides(params: &mut Params, overrides: &BTreeMap<String, toml::Value>) -> Vec<String> {
    let mut warnings = Vec::new();
    for (id, val) in overrides {
        let Some(m) = PARAMS_META.iter().find(|m| m.id == id) else {
            warnings.push(format!("unknown parameter '{id}' ignored"));
            continue;
        };
        match from_value(m.ty, val) {
            Some(v) if params.set(id, v) => {}
            _ => warnings.push(format!("invalid value for parameter '{id}' ignored")),
        }
    }
    warnings
}

/// `%APPDATA%\ultimateCRT`, or `None` when `APPDATA` is not set.
pub fn default_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("ultimateCRT"))
}

/// Result of [`load`].
#[derive(Debug)]
pub struct Loaded {
    pub settings: Settings,
    /// Set when the file was unreadable or invalid (defaults are returned, the file kept as `.bak`).
    pub warning: Option<String>,
}

/// Loads `dir\settings.toml`. A missing file gives defaults without a warning.
pub fn load(dir: &Path) -> Loaded {
    let path = dir.join(SETTINGS_FILE);
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Loaded {
                settings: Settings::default(),
                warning: None,
            }
        }
        Err(e) => return corrupt(dir, &path, e.to_string()),
    };
    match Settings::from_toml(&text) {
        Ok(settings) => Loaded {
            settings,
            warning: None,
        },
        Err(e) => corrupt(dir, &path, e),
    }
}

fn corrupt(dir: &Path, path: &Path, reason: String) -> Loaded {
    let bak = dir.join(BACKUP_FILE);
    let kept = fs::copy(path, &bak).is_ok();
    let warning = format!(
        "{} is invalid ({reason}); defaults used{}",
        path.display(),
        if kept {
            format!(", the file was kept as {}", bak.display())
        } else {
            String::new()
        }
    );
    Loaded {
        settings: Settings::default(),
        warning: Some(warning),
    }
}

/// Writes `dir\settings.toml` atomically (temporary file in the same directory, flushed, then renamed).
pub fn save(dir: &Path, settings: &Settings) -> crate::Res<()> {
    fs::create_dir_all(dir)?;
    let text = settings.to_toml()?;
    let tmp = dir.join("settings.toml.tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, dir.join(SETTINGS_FILE))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ucrt-settings-{name}-{}", std::process::id()));
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
        s.host.panel_mm = Some([527.0, 296.5]);
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
        });
        let mut p = Params::default();
        let m = some_id();
        let ParamValue::F32(d) = m.default else { unreachable!() };
        assert!(p.set(m.id, ParamValue::F32(d + 0.25)));
        s.global.overrides = overrides_from_params(&p);
        save(&dir, &s).unwrap();
        assert!(!dir.join("settings.toml.tmp").exists());
        let back = load(&dir);
        assert!(back.warning.is_none());
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
        assert_eq!(
            fs::read_to_string(dir.join(BACKUP_FILE)).unwrap(),
            "this is = = not toml [["
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn newer_schema_is_set_aside() {
        let dir = temp_dir("newer");
        fs::write(dir.join(SETTINGS_FILE), format!("version = {}\n", SCHEMA_VERSION + 1)).unwrap();
        let l = load(&dir);
        assert!(l.warning.is_some());
        assert!(dir.join(BACKUP_FILE).exists());
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
    fn bad_overrides_warn_and_are_skipped() {
        let mut o = BTreeMap::new();
        o.insert("no_such_param".to_string(), toml::Value::Float(1.0));
        o.insert(some_id().id.to_string(), toml::Value::String("x".into()));
        let mut p = Params::default();
        assert_eq!(apply_overrides(&mut p, &o).len(), 2);
    }

    #[test]
    fn every_parameter_type_round_trips_through_overrides() {
        for m in PARAMS_META {
            let v = to_value(m.default);
            assert_eq!(from_value(m.ty, &v), Some(m.default), "{}", m.id);
        }
    }

    #[test]
    fn hotkey_defaults_reserve_f11_and_f12_and_fill_gaps() {
        let s = Settings::from_toml("version = 1\n[global.hotkeys]\nquit = \"Ctrl+Q\"\n").unwrap();
        assert_eq!(s.global.hotkeys["toggle_fullscreen"], "F11");
        assert_eq!(s.global.hotkeys["toggle_menu"], "F12");
        assert_eq!(s.global.hotkeys["quit"], "Ctrl+Q");
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

    #[test]
    fn host_profile_override_wins() {
        let mut h = HostProfile::default();
        assert_eq!(h.effective_panel_mm(), Some([530.0, 300.0]));
        h.panel_mm_override = None;
        assert_eq!(h.effective_panel_mm(), None);
        h.panel_mm = Some([1.0, 2.0]);
        assert_eq!(h.effective_panel_mm(), Some([1.0, 2.0]));
    }
}
