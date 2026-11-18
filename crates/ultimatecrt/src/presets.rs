//! Tube presets as data files (T-50, QUESTIONS PHY-4 and PHY-2 option D, ARCHITECTURE A13).
//!
//! A preset is a TOML file of `presets/tubes/` (embedded in the binary with `include_str!`) or of the user
//! directory `<settings dir>\presets\` (a user file with the same `id` replaces the shipped one):
//!
//! ```toml
//! schema = 1
//! id = "pvm-20m4u"                      # lower case letters, digits and '-'
//! name = "Sony PVM-20M4U (professional monitor)"
//! label = "based on published data for the Sony PVM-20M4U"
//! notes = "..."                         # confidence summary, defaults kept, source remarks
//! [params]                              # parameter id -> value, in physical units as in params/*.toml
//! mask_pitch_mm = 0.31
//! mask_type = "aperture_grille"         # enums by value name
//! [provenance]                          # one entry per [params] key: "<S|D|K|E>: <source>"
//! mask_pitch_mm = "S: CRT.md Sony professional table"
//! ```
//!
//! Validation (`Preset::parse`): an unknown top-level key, an unknown parameter id, a value of the wrong type,
//! an enum name that does not exist, a missing or malformed provenance entry, or a schema other than
//! [`SCHEMA_VERSION`] is an error that names the file and the key. A value outside the soft range of the
//! parameter table is a warning (`Preset::warnings`; the value is kept), as outside values are allowed in the
//! UI. Applying a preset sets its values onto `Params` with `Params::set`; ids it does not list keep their
//! table default, so a preset is always applied onto the defaults (`Preset::params`).
//!
//! Host display (PHY-2 option D, P-1): `Preset::px_per_triad` and `option_d_suggestions` give the host pixels
//! per triad for a host output size, `Preset::host_note` the label "mask not resolvable on this host" below
//! [`OPTION_D_THRESHOLD_PX_PER_TRIAD`] (the oracle `crt_model::mask::option_d` is used for the list).
//! The tube is placed as in `placement.rs`: height fraction of the output, limited by the width.
//!
//! `amp_k` (the amplifier sub-samples per source pixel) is a `FrameConstants` field, not a parameter, so a
//! preset cannot carry it: [`recommended_amp_k`] gives the value its bandwidth needs.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use crt_model::mask::{option_d, tier, Tier, OPTION_D_THRESHOLD_PX_PER_TRIAD};

use crate::{
    params::{meta, Confidence, ParamType, ParamValue, Params, MASK_TYPE_DELTA},
    settings::{from_value, to_value},
};

/// Preset file schema version.
pub const SCHEMA_VERSION: u32 = 1;

/// Shown for a preset whose mask the host cannot resolve (PLAN P-1).
pub const NOT_RESOLVABLE_NOTE: &str = "mask not resolvable on this host";

/// Bandwidth above which the box-integral amplifier needs K = 16 sub-samples (K = 4 over-dims isolated
/// vertical lines above about 5 MHz: 8 MHz 14.99 % against 12.36 % continuous, K = 16 12.68 %).
pub const AMP_K_HIGH_BANDWIDTH_MHZ: f32 = 5.0;

/// Amplifier sub-samples per native pixel that `params` needs: 16 above [`AMP_K_HIGH_BANDWIDTH_MHZ`], else the
/// pipeline default 4 (`FrameConstants::amp_k`, which a preset cannot set).
pub fn recommended_amp_k(params: &Params) -> i32 {
    if params.amp_bandwidth_mhz > AMP_K_HIGH_BANDWIDTH_MHZ {
        16
    } else {
        crate::gpu::AMP_K as i32
    }
}

/// The shipped presets, in menu order: (file name, text).
const EMBEDDED: &[(&str, &str)] = &[
    (
        "01-pvm-20m4u.toml",
        include_str!("../../../presets/tubes/01-pvm-20m4u.toml"),
    ),
    (
        "02-bvm-20f1u.toml",
        include_str!("../../../presets/tubes/02-bvm-20f1u.toml"),
    ),
    (
        "03-kv-trinitron-tv.toml",
        include_str!("../../../presets/tubes/03-kv-trinitron-tv.toml"),
    ),
    (
        "04-gdm-f520.toml",
        include_str!("../../../presets/tubes/04-gdm-f520.toml"),
    ),
    (
        "05-slot-tv-26in.toml",
        include_str!("../../../presets/tubes/05-slot-tv-26in.toml"),
    ),
    (
        "06-tv-13in.toml",
        include_str!("../../../presets/tubes/06-tv-13in.toml"),
    ),
    (
        "07-cromaclear-17in.toml",
        include_str!("../../../presets/tubes/07-cromaclear-17in.toml"),
    ),
    (
        "08-arcade-19in.toml",
        include_str!("../../../presets/tubes/08-arcade-19in.toml"),
    ),
    (
        "09-vga-15in.toml",
        include_str!("../../../presets/tubes/09-vga-15in.toml"),
    ),
    (
        "10-delta-17in.toml",
        include_str!("../../../presets/tubes/10-delta-17in.toml"),
    ),
];

/// A preset file that cannot be used: the message names the file and, when it applies, the key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetError {
    pub file: String,
    pub message: String,
}

impl std::fmt::Display for PresetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "preset {}: {}", self.file, self.message)
    }
}

impl std::error::Error for PresetError {}

/// Confidence tag and source of one preset value (`CRT.md` tags).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provenance {
    pub tag: Confidence,
    pub source: String,
}

/// Where a preset comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    Embedded,
    User(PathBuf),
}

/// One tube preset.
#[derive(Debug, Clone, PartialEq)]
pub struct Preset {
    pub id: String,
    pub name: String,
    pub label: String,
    pub notes: String,
    /// File name (embedded) or path (user), as used in messages.
    pub file: String,
    pub origin: Origin,
    /// Parameter id and value, in file order of the table (sorted by id).
    pub values: Vec<(&'static str, ParamValue)>,
    pub provenance: BTreeMap<String, Provenance>,
    /// One text per value outside its soft range.
    pub warnings: Vec<String>,
}

fn err(file: &str, message: impl Into<String>) -> PresetError {
    PresetError {
        file: file.to_string(),
        message: message.into(),
    }
}

fn tag_of(c: char) -> Option<Confidence> {
    Some(match c {
        'S' => Confidence::Sourced,
        'D' => Confidence::Derived,
        'K' => Confidence::Known,
        'E' => Confidence::Estimate,
        _ => return None,
    })
}

/// Components of a value, for the soft range check.
fn components(v: ParamValue) -> Vec<f64> {
    match v {
        ParamValue::F32(x) => vec![f64::from(x)],
        ParamValue::I32(x) => vec![f64::from(x)],
        ParamValue::Vec2(a) => a.iter().map(|x| f64::from(*x)).collect(),
        ParamValue::Vec3(a) => a.iter().map(|x| f64::from(*x)).collect(),
        ParamValue::Vec4(a) => a.iter().map(|x| f64::from(*x)).collect(),
        ParamValue::Bool(_) | ParamValue::Enum(_) => Vec::new(),
    }
}

fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn string_field(table: &toml::Table, key: &str, file: &str) -> Result<String, PresetError> {
    match table.get(key) {
        Some(toml::Value::String(s)) if !s.trim().is_empty() => Ok(s.clone()),
        Some(_) => Err(err(file, format!("'{key}' must be a non-empty string"))),
        None => Err(err(file, format!("missing key '{key}'"))),
    }
}

impl Preset {
    /// Parses and validates a preset file. `file` names it in messages.
    pub fn parse(file: &str, text: &str, origin: Origin) -> Result<Preset, PresetError> {
        let table: toml::Table = text.parse().map_err(|e: toml::de::Error| err(file, e.to_string()))?;
        for key in table.keys() {
            if !["schema", "id", "name", "label", "notes", "params", "provenance"].contains(&key.as_str()) {
                return Err(err(file, format!("unknown key '{key}'")));
            }
        }
        match table.get("schema") {
            Some(toml::Value::Integer(v)) if *v == i64::from(SCHEMA_VERSION) => {}
            Some(toml::Value::Integer(v)) => {
                return Err(err(
                    file,
                    format!("schema version {v} is not supported (expected {SCHEMA_VERSION})"),
                ))
            }
            Some(_) => return Err(err(file, "'schema' must be an integer")),
            None => return Err(err(file, "missing key 'schema'")),
        }
        let id = string_field(&table, "id", file)?;
        if !is_valid_id(&id) {
            return Err(err(
                file,
                format!("id '{id}' must use lower case letters, digits and '-'"),
            ));
        }
        let name = string_field(&table, "name", file)?;
        let label = string_field(&table, "label", file)?;
        let notes = table
            .get("notes")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let params = table
            .get("params")
            .and_then(|v| v.as_table())
            .ok_or_else(|| err(file, "missing table [params]"))?;
        let prov = table
            .get("provenance")
            .and_then(|v| v.as_table())
            .ok_or_else(|| err(file, "missing table [provenance]"))?;

        let mut values = Vec::new();
        let mut warnings = Vec::new();
        for (key, raw) in params {
            let m = meta(key).ok_or_else(|| err(file, format!("unknown parameter id '{key}'")))?;
            let wrong = || err(file, format!("parameter '{key}': expected a value of type {:?}", m.ty));
            let value = match (m.ty, raw) {
                (ParamType::Enum, toml::Value::String(s)) => {
                    let i =
                        m.values.iter().position(|v| v == s).ok_or_else(|| {
                            err(file, format!("parameter '{key}': '{s}' is not one of {:?}", m.values))
                        })?;
                    ParamValue::Enum(i as u32)
                }
                (ParamType::Enum, _) => return Err(wrong()),
                (ty, raw) => from_value(ty, raw).ok_or_else(wrong)?,
            };
            if let (Some((lo, hi)), true) = (m.soft_range, !components(value).is_empty()) {
                // Bounds in f32, like the table's own range test: 1.6 as f32 is above 1.6 as f64.
                if components(value)
                    .iter()
                    .any(|c| !((lo as f32)..=(hi as f32)).contains(&(*c as f32)))
                {
                    warnings.push(format!(
                        "{file}: parameter '{key}' = {raw} is outside its soft range {lo} to {hi} {}",
                        m.unit
                    ));
                }
            }
            values.push((m.id, value));
        }
        let mut provenance = BTreeMap::new();
        for (key, raw) in prov {
            if !params.contains_key(key) {
                return Err(err(file, format!("provenance for '{key}', which is not in [params]")));
            }
            let bad = || err(file, format!("provenance of '{key}' must read \"<S|D|K|E>: <source>\""));
            let s = raw.as_str().ok_or_else(bad)?;
            let (head, source) = s.split_once(':').ok_or_else(bad)?;
            let mut chars = head.chars();
            let tag = chars.next().and_then(tag_of).filter(|_| chars.next().is_none());
            let (Some(tag), true) = (tag, !source.trim().is_empty()) else {
                return Err(bad());
            };
            provenance.insert(
                key.clone(),
                Provenance {
                    tag,
                    source: source.trim().to_string(),
                },
            );
        }
        if let Some(key) = params.keys().find(|k| !provenance.contains_key(*k)) {
            return Err(err(file, format!("parameter '{key}' has no provenance entry")));
        }
        Ok(Preset {
            id,
            name,
            label,
            notes,
            file: file.to_string(),
            origin,
            values,
            provenance,
            warnings,
        })
    }

    /// Sets the preset values onto `params` (other ids are left as they are).
    pub fn apply(&self, params: &mut Params) {
        for (id, v) in &self.values {
            // Every value was validated against the table at parse time.
            let ok = params.set(id, *v);
            debug_assert!(ok, "preset {}: '{id}' rejected", self.file);
        }
    }

    /// The table defaults with the preset applied.
    pub fn params(&self) -> Params {
        let mut p = Params::default();
        self.apply(&mut p);
        p
    }

    /// The overrides of `current` against this preset: the ids whose value differs from the preset applied
    /// onto the defaults (T-53 "diff vs preset", T-51 settings overrides).
    pub fn diff(&self, current: &Params) -> BTreeMap<String, toml::Value> {
        diff_overrides(&self.params(), current)
    }

    /// The preset with `overrides` (as stored in the settings) applied; returns the parameters and one
    /// warning per override that could not be applied.
    pub fn with_overrides(&self, overrides: &BTreeMap<String, toml::Value>) -> (Params, Vec<String>) {
        let mut p = self.params();
        let warnings = crate::settings::apply_overrides(&mut p, overrides);
        (p, warnings)
    }

    /// Host pixels per millimetre of the tube on a `host` output (width, height) in pixels: the tube raster
    /// takes `tube_height_fraction` of the height and is limited by the width (`placement.rs`).
    pub fn px_per_mm(&self, host: (u32, u32)) -> f64 {
        host_px_per_mm(&self.params(), host)
    }

    /// Horizontal triad pitch in mm (the delta pitch P times 0.866).
    pub fn triad_pitch_mm(&self) -> f64 {
        triad_pitch_mm(&self.params())
    }

    /// Triads across the visible width.
    pub fn triads_across(&self) -> f64 {
        triads_across(&self.params())
    }

    /// Host pixels per triad on a `host` output.
    pub fn px_per_triad(&self, host: (u32, u32)) -> f64 {
        let p = self.params();
        host_px_per_mm(&p, host) * triad_pitch_mm(&p)
    }

    /// Accuracy tier of the mask on `host` (PHY-2).
    pub fn tier(&self, host: (u32, u32)) -> Tier {
        tier(self.px_per_triad(host))
    }

    /// "mask not resolvable on this host" when the host gets fewer px per triad than
    /// [`OPTION_D_THRESHOLD_PX_PER_TRIAD`] (PLAN P-1), else `None`.
    pub fn host_note(&self, host: (u32, u32)) -> Option<&'static str> {
        (self.px_per_triad(host) < OPTION_D_THRESHOLD_PX_PER_TRIAD).then_some(NOT_RESOLVABLE_NOTE)
    }

    /// Bandwidth-dependent `amp_k` for this preset ([`recommended_amp_k`]).
    pub fn recommended_amp_k(&self) -> i32 {
        recommended_amp_k(&self.params())
    }
}

fn host_px_per_mm(p: &Params, host: (u32, u32)) -> f64 {
    let h_mm = f64::from(p.tube_visible_height_mm);
    let w_mm = h_mm * f64::from(p.tube_aspect);
    let by_height = f64::from(host.1) * f64::from(p.tube_height_fraction) / h_mm;
    let by_width = f64::from(host.0) / w_mm;
    by_height.min(by_width)
}

fn triad_pitch_mm(p: &Params) -> f64 {
    let pitch = f64::from(p.mask_pitch_mm);
    if p.mask_type == MASK_TYPE_DELTA {
        pitch * 3.0_f64.sqrt() / 2.0
    } else {
        pitch
    }
}

fn triads_across(p: &Params) -> f64 {
    f64::from(p.tube_visible_height_mm) * f64::from(p.tube_aspect) / triad_pitch_mm(p)
}

/// The ids whose value in `params` differs from `base`, with their values (as `settings` stores overrides).
pub fn diff_overrides(base: &Params, params: &Params) -> BTreeMap<String, toml::Value> {
    let mut out = BTreeMap::new();
    for m in crate::params::PARAMS_META {
        if let (Some(a), Some(b)) = (base.get(m.id), params.get(m.id)) {
            if a != b {
                out.insert(m.id.to_string(), to_value(b));
            }
        }
    }
    out
}

/// Option D (PHY-2): the presets of `presets` (indices) whose mask the `host` output resolves, that is at
/// least [`OPTION_D_THRESHOLD_PX_PER_TRIAD`] px per triad, computed with `crt_model::mask::option_d`.
pub fn option_d_suggestions(presets: &[Preset], host: (u32, u32)) -> Vec<usize> {
    // The tubes differ in size, so each one is placed on its own: its image is `visible width * px_per_mm`
    // pixels wide and holds `triads across` triads, which is what the oracle takes.
    presets
        .iter()
        .enumerate()
        .filter(|(_, p)| {
            let params = p.params();
            let triads = triads_across(&params);
            let width_px = f64::from(params.tube_visible_height_mm)
                * f64::from(params.tube_aspect)
                * host_px_per_mm(&params, host);
            !option_d(&[(0, triads)], width_px, OPTION_D_THRESHOLD_PX_PER_TRIAD).is_empty()
        })
        .map(|(i, _)| i)
        .collect()
}

/// The presets, shipped ones first, then user ones, with the problems met while loading them.
#[derive(Debug, Clone, Default)]
pub struct Library {
    presets: Vec<Preset>,
    /// Files that were skipped (errors) and values outside soft ranges (warnings), in load order.
    pub warnings: Vec<String>,
}

/// The directory of user presets next to `settings.toml`.
pub fn user_dir(settings_dir: &Path) -> PathBuf {
    settings_dir.join("presets")
}

impl Library {
    /// The presets embedded in the binary. A shipped file that fails to parse is reported in `warnings`
    /// (the test suite requires none).
    pub fn embedded() -> Library {
        let mut lib = Library::default();
        for (file, text) in EMBEDDED {
            lib.add(Preset::parse(&format!("presets/tubes/{file}"), text, Origin::Embedded));
        }
        lib
    }

    /// The embedded presets plus the `*.toml` files of `dir` (sorted by file name). A user preset with the id
    /// of a shipped one replaces it in place; others are appended. A broken user file is skipped with a
    /// warning naming the file and key.
    pub fn load(dir: Option<&Path>) -> Library {
        let mut lib = Library::embedded();
        let Some(dir) = dir else { return lib };
        let Ok(rd) = fs::read_dir(dir) else { return lib };
        let mut files: Vec<PathBuf> = rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("toml")))
            .collect();
        files.sort();
        for path in files {
            let shown = path.display().to_string();
            match fs::read_to_string(&path) {
                Ok(text) => lib.add(Preset::parse(&shown, &text, Origin::User(path.clone()))),
                Err(e) => lib.warnings.push(format!("preset {shown}: {e}")),
            }
        }
        lib
    }

    fn add(&mut self, parsed: Result<Preset, PresetError>) {
        match parsed {
            Ok(p) => {
                self.warnings.extend(p.warnings.iter().cloned());
                match self.presets.iter_mut().find(|q| q.id == p.id) {
                    Some(slot) => *slot = p,
                    None => self.presets.push(p),
                }
            }
            Err(e) => self.warnings.push(e.to_string()),
        }
    }

    /// The presets in menu order.
    pub fn list(&self) -> &[Preset] {
        &self.presets
    }

    pub fn find(&self, id: &str) -> Option<&Preset> {
        self.presets.iter().find(|p| p.id == id)
    }

    fn position(&self, current: Option<&str>) -> Option<usize> {
        current.and_then(|id| self.presets.iter().position(|p| p.id == id))
    }

    /// The preset after `current` in menu order, wrapping around (T-54 "next"). With no or an unknown
    /// current preset, the first one.
    pub fn next(&self, current: Option<&str>) -> Option<&Preset> {
        let n = self.presets.len();
        let i = self.position(current).map_or(0, |i| (i + 1) % n.max(1));
        self.presets.get(i)
    }

    /// The preset before `current`, wrapping around (T-54 "previous"). With no or an unknown current preset,
    /// the last one.
    pub fn previous(&self, current: Option<&str>) -> Option<&Preset> {
        let n = self.presets.len();
        let i = self.position(current).map_or(n.wrapping_sub(1), |i| (i + n - 1) % n);
        self.presets.get(i)
    }

    /// Indices of the presets that `host` resolves ([`option_d_suggestions`]).
    pub fn option_d(&self, host: (u32, u32)) -> Vec<usize> {
        option_d_suggestions(&self.presets, host)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::PARAMS_META;
    use crt_model::beam::SpotGrowth;

    const HOST: (u32, u32) = (2560, 1440);

    fn lib() -> Library {
        let l = Library::embedded();
        assert!(l.warnings.is_empty(), "{:?}", l.warnings);
        l
    }

    fn sample(extra: &str) -> String {
        format!(
            "schema = 1\nid = \"x\"\nname = \"X\"\nlabel = \"l\"\n[params]\nexposure = 0.5\n{extra}\n[provenance]\nexposure = \"E: test\"\n"
        )
    }

    #[test]
    fn ten_presets_load_in_order_with_unique_ids() {
        let l = lib();
        assert_eq!(l.list().len(), 10);
        let files: Vec<&str> = l.list().iter().map(|p| p.file.as_str()).collect();
        let mut sorted = files.clone();
        sorted.sort();
        assert_eq!(files, sorted);
        let mut ids: Vec<&str> = l.list().iter().map(|p| p.id.as_str()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 10);
        for p in l.list() {
            assert!(p.label.starts_with("based on published data for"), "{}", p.id);
            assert!(!p.notes.is_empty(), "{}", p.id);
            assert_eq!(p.origin, Origin::Embedded);
        }
    }

    #[test]
    fn every_value_has_provenance_and_is_inside_its_soft_range() {
        for p in lib().list() {
            assert_eq!(p.values.len(), p.provenance.len(), "{}", p.id);
            assert!(p.warnings.is_empty(), "{}: {:?}", p.id, p.warnings);
            for (id, v) in &p.values {
                assert!(p.provenance.contains_key(*id), "{}: {id}", p.id);
                let m = meta(id).unwrap();
                if let Some((lo, hi)) = m.soft_range {
                    for c in components(*v) {
                        assert!(
                            ((lo as f32)..=(hi as f32)).contains(&(c as f32)),
                            "{}: {id} = {c}",
                            p.id
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn presets_follow_the_recorded_policies() {
        for p in lib().list() {
            let q = p.params();
            assert!(!q.output_match_srgb, "{}: tube-faithful (COL-2)", p.id);
            assert_eq!(q.output_dither, crate::params::OUTPUT_DITHER_NOISE, "{}", p.id);
            assert!(
                q.amp_bandwidth_mhz != 100.0 || p.id == "vga-15in",
                "{}: own bandwidth",
                p.id
            );
            // Dampers: only grilles have them.
            let grille = q.mask_type == crate::params::MASK_TYPE_APERTURE_GRILLE;
            assert_eq!(q.mask_damper_count > 0, grille, "{}", p.id);
            // White: no clip. The linear sRGB maximum of the tube white at Y = 1, times exposure, is <= 1.
            let w = xy_to_srgb_max(q.tube_white_xy);
            assert!(w * f64::from(q.exposure) <= 1.0 + 1e-3, "{}: white clips ({w})", p.id);
            // Slow-rise direction on consumer amplifiers (r < 1 dims isolated pixels), symmetric elsewhere.
            assert!(q.amp_fall_rise_ratio <= 1.0, "{}", p.id);
        }
        let l = lib();
        assert_eq!(l.find("pvm-20m4u").unwrap().params().mask_damper_count, 2);
        assert_eq!(l.find("bvm-20f1u").unwrap().params().mask_damper_count, 2);
        assert_eq!(l.find("kv-trinitron-tv").unwrap().params().exposure, 0.74);
    }

    /// Largest linear Rec.709 channel of the white (x, y) at Y = 1.
    fn xy_to_srgb_max(xy: [f32; 2]) -> f64 {
        let (x, y) = (f64::from(xy[0]), f64::from(xy[1]));
        let (cx, cz) = (x / y, (1.0 - x - y) / y);
        let r = 3.2406 * cx - 1.5372 - 0.4986 * cz;
        let g = -0.9689 * cx + 1.8758 + 0.0415 * cz;
        let b = 0.0557 * cx - 0.2040 + 1.0570 * cz;
        r.max(g).max(b)
    }

    #[test]
    fn d93_whites_need_exposure_at_most_0_745() {
        let d93 = xy_to_srgb_max([0.2831, 0.2971]);
        assert!((1.0 / d93 - 0.7444).abs() < 5e-4, "{}", 1.0 / d93);
        for p in lib().list() {
            if p.params().tube_white_xy == [0.2831, 0.2971] {
                assert!(p.params().exposure <= 0.745, "{}", p.id);
            }
        }
    }

    /// Spot growth of every preset: the law keeps its own exponent (no raise to the zero-floor bound) and the
    /// floor d0^2 is not negative, through `crt_model::beam::SpotGrowth` (T-36b).
    #[test]
    fn spot_growth_triples_keep_a_non_negative_floor() {
        for p in lib().list() {
            let q = p.params();
            let g = SpotGrowth {
                fwhm_mid: f64::from(q.beam_fwhm_mid_mm),
                fwhm_peak: f64::from(q.beam_fwhm_peak_mm),
                n: f64::from(q.beam_growth_exponent),
            };
            assert!(
                (g.exponent() - g.n).abs() < 1e-12,
                "{}: n {} would be raised to {}",
                p.id,
                g.n,
                g.exponent()
            );
            let (d0_sq, k) = g.floor_sq_and_k();
            assert!(d0_sq >= 0.0 && k > 0.0, "{}: floor {d0_sq}, k {k}", p.id);
            assert!(g.fwhm(1.0) >= g.fwhm(0.1895), "{}", p.id);
        }
        // The bound of the T-36b notes for the default 0.45 -> 0.8 mm.
        let g = SpotGrowth {
            fwhm_mid: 0.45,
            fwhm_peak: 0.8,
            n: 0.34,
        };
        assert!(g.exponent() > 0.3458, "0.34 is below the bound for 0.45 -> 0.8");
        // The sourced Hitachi triple 0.45 / 0.70 with n = 0.27 is a valid zero-floor-safe monitor law.
        let h = SpotGrowth {
            fwhm_mid: 0.45,
            fwhm_peak: 0.70,
            n: 0.27,
        };
        assert!((h.exponent() - 0.27).abs() < 1e-12 && h.floor_sq_and_k().0 > 0.0);
    }

    #[test]
    fn high_bandwidth_presets_recommend_amp_k_16() {
        let l = lib();
        for p in l.list() {
            let b = p.params().amp_bandwidth_mhz;
            assert_eq!(p.recommended_amp_k(), if b > 5.0 { 16 } else { 4 }, "{}", p.id);
        }
        assert_eq!(l.find("tv-13in").unwrap().recommended_amp_k(), 4);
        assert_eq!(l.find("pvm-20m4u").unwrap().recommended_amp_k(), 16);
        assert_eq!(recommended_amp_k(&Params::default()), 16, "the 100 MHz default");
    }

    /// PHY-4 table, 1920 px wide image (1440 px high): px per triad within 10 % of the recorded values.
    #[test]
    fn px_per_triad_matches_the_phy4_table() {
        let want = [1.5, 1.5, 2.9, 1.0, 2.9, 4.4, 1.5, 4.0, 1.7, 1.3];
        for (p, w) in lib().list().iter().zip(want) {
            let got = p.px_per_triad(HOST);
            assert!((got / w - 1.0).abs() <= 0.10, "{}: {got} against {w}", p.id);
        }
    }

    #[test]
    fn option_d_for_2560x1440_is_presets_3_5_6_8() {
        let l = lib();
        let ids: Vec<usize> = l.option_d(HOST).iter().map(|i| i + 1).collect();
        assert_eq!(ids, vec![3, 5, 6, 8]);
        // The mask oracle with the PHY-4 triad counts gives the same set.
        let counts = [
            1245.0, 1287.0, 660.0, 1827.0, 660.0, 440.0, 1300.0, 480.0, 1157.0, 1443.0,
        ];
        let oracle: Vec<u32> = option_d(
            &counts
                .iter()
                .enumerate()
                .map(|(i, c)| (i as u32 + 1, *c))
                .collect::<Vec<_>>(),
            1920.0,
            OPTION_D_THRESHOLD_PX_PER_TRIAD,
        );
        assert_eq!(oracle, vec![3, 5, 6, 8]);
        // A 4K host resolves more of them, a 1080p host fewer.
        assert_eq!(l.option_d((3840, 2160)).len(), 4);
        assert_eq!(l.option_d((1920, 1080)), vec![5, 7], "13 in TV and arcade only");
    }

    #[test]
    fn host_note_labels_the_unresolvable_presets() {
        let l = lib();
        let noted: Vec<usize> = l
            .list()
            .iter()
            .enumerate()
            .filter(|(_, p)| p.host_note(HOST) == Some(NOT_RESOLVABLE_NOTE))
            .map(|(i, _)| i + 1)
            .collect();
        // PLAN P-1: 1, 2, 4, 7, 9 and 10 (PHY-4 px per triad up to 1.7), plus nothing else.
        assert_eq!(noted, vec![1, 2, 4, 7, 9, 10]);
        assert_eq!(l.list()[2].tier(HOST), Tier::C);
        assert_eq!(l.list()[5].tier(HOST), Tier::B);
    }

    #[test]
    fn diff_against_a_preset_round_trips() {
        let l = lib();
        for p in l.list() {
            assert!(p.diff(&p.params()).is_empty(), "{}", p.id);
            let mut cur = p.params();
            assert!(cur.set("exposure", ParamValue::F32(0.6)));
            assert!(cur.set("beam_tail_weight", ParamValue::F32(0.05)));
            assert!(cur.set("gun_white_balance", ParamValue::Bool(true)));
            assert!(cur.set("tube_red_xy", ParamValue::Vec2([0.64, 0.33])));
            let d = p.diff(&cur);
            assert!(d.len() <= 4 && d.contains_key("beam_tail_weight"), "{}: {d:?}", p.id);
            let (back, warn) = p.with_overrides(&d);
            assert!(warn.is_empty());
            assert_eq!(back, cur, "{}", p.id);
            // Against the defaults instead, the preset keys appear.
            let all = diff_overrides(&Params::default(), &p.params());
            for (id, _) in &p.values {
                let default = PARAMS_META.iter().find(|m| m.id == *id).unwrap().default;
                assert_eq!(all.contains_key(*id), p.params().get(id) != Some(default));
            }
        }
    }

    #[test]
    fn next_and_previous_cycle() {
        let l = lib();
        let ids: Vec<String> = l.list().iter().map(|p| p.id.clone()).collect();
        let mut cur = None;
        let mut seen = Vec::new();
        for _ in 0..10 {
            let n = l.next(cur).unwrap();
            seen.push(n.id.clone());
            cur = Some(n.id.as_str());
        }
        assert_eq!(seen, ids);
        assert_eq!(l.next(cur).unwrap().id, ids[0], "wraps");
        assert_eq!(l.previous(Some(&ids[0])).unwrap().id, ids[9], "wraps back");
        assert_eq!(l.previous(Some(&ids[4])).unwrap().id, ids[3]);
        assert_eq!(l.previous(None).unwrap().id, ids[9]);
        assert_eq!(l.next(Some("unknown")).unwrap().id, ids[0]);
        assert!(Library::default().next(None).is_none());
        assert!(Library::default().previous(None).is_none());
    }

    #[test]
    fn unknown_id_error_names_file_and_id() {
        let e = Preset::parse("my.toml", &sample("no_such_parameter = 1.0"), Origin::Embedded).unwrap_err();
        assert!(
            e.to_string().contains("my.toml") && e.to_string().contains("no_such_parameter"),
            "{e}"
        );
    }

    #[test]
    fn type_enum_schema_and_provenance_errors() {
        let parse = |t: &str| Preset::parse("f.toml", t, Origin::Embedded);
        let e = parse(&sample("mask_pitch_mm = \"wide\"")).unwrap_err();
        assert!(e.message.contains("mask_pitch_mm") && e.message.contains("type"), "{e}");
        let e = parse(&sample("mask_type = \"hexagon\"")).unwrap_err();
        assert!(e.message.contains("mask_type") && e.message.contains("hexagon"), "{e}");
        let e = parse(&sample("tube_red_xy = [0.1, 0.2, 0.3]")).unwrap_err();
        assert!(e.message.contains("tube_red_xy"), "{e}");
        let e = parse(&sample("").replace("schema = 1", "schema = 2")).unwrap_err();
        assert!(e.message.contains("schema version 2"), "{e}");
        let e = parse(&sample("").replace("schema = 1\n", "")).unwrap_err();
        assert!(e.message.contains("schema"), "{e}");
        let e = parse(&sample("mask_pitch_mm = 0.3")).unwrap_err();
        assert!(
            e.message.contains("mask_pitch_mm") && e.message.contains("provenance"),
            "{e}"
        );
        let e = parse(&sample("").replace("E: test", "Q: test")).unwrap_err();
        assert!(e.message.contains("exposure"), "{e}");
        let e = parse(&sample("").replace("[provenance]", "[provenance]\nghost = \"E: x\"")).unwrap_err();
        assert!(e.message.contains("ghost"), "{e}");
        let e = parse(&sample("").replace("schema = 1", "schema = 1\nextra = 1")).unwrap_err();
        assert!(e.message.contains("unknown"), "{e}");
        // A valid file parses; an enum is given by name.
        let p = parse(&sample("mask_type = \"slot_mask\"")).unwrap_err();
        assert!(p.message.contains("mask_type"), "no provenance for mask_type");
        let ok = sample("mask_type = \"slot_mask\"")
            .replace("exposure = \"E: test\"", "exposure = \"E: test\"\nmask_type = \"S: t\"");
        let p = parse(&ok).unwrap();
        assert_eq!(p.params().mask_type, crate::params::MASK_TYPE_SLOT_MASK);
        assert_eq!(p.params().exposure, 0.5);
    }

    #[test]
    fn out_of_range_is_a_warning_and_the_value_is_kept() {
        let t = sample("").replace("exposure = 0.5", "exposure = 50.0");
        let p = Preset::parse("hi.toml", &t, Origin::Embedded).unwrap();
        assert_eq!(p.warnings.len(), 1);
        assert!(p.warnings[0].contains("hi.toml") && p.warnings[0].contains("exposure"));
        assert_eq!(p.params().exposure, 50.0);
    }

    #[test]
    fn user_directory_overrides_by_id_and_skips_broken_files() {
        let dir = std::env::temp_dir().join(format!("ucrt-presets-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let mine = sample("")
            .replace("id = \"x\"", "id = \"pvm-20m4u\"")
            .replace("exposure = 0.5", "exposure = 0.9");
        fs::write(dir.join("a.toml"), mine).unwrap();
        fs::write(dir.join("b.toml"), sample("")).unwrap();
        fs::write(dir.join("broken.toml"), sample("bogus_id = 1")).unwrap();
        fs::write(dir.join("note.txt"), "ignored").unwrap();
        let l = Library::load(Some(&dir));
        assert_eq!(l.list().len(), 11);
        let p = l.find("pvm-20m4u").unwrap();
        assert!(matches!(p.origin, Origin::User(_)));
        assert_eq!(p.params().exposure, 0.9);
        assert_eq!(l.list()[0].id, "pvm-20m4u", "replaced in place");
        assert_eq!(l.list()[10].id, "x");
        assert_eq!(l.warnings.len(), 1);
        assert!(l.warnings[0].contains("broken.toml") && l.warnings[0].contains("bogus_id"));
        assert_eq!(Library::load(Some(&dir.join("missing"))).list().len(), 10);
        assert_eq!(user_dir(Path::new("C:\\a")), Path::new("C:\\a").join("presets"));
        let _ = fs::remove_dir_all(&dir);
    }
}
