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
//! `amp_samples` (the amplifier sub-samples per source pixel, `FrameConstants::amp_k`) is a parameter, so a
//! preset carries it: [`recommended_amp_k`] gives the value the amplifier blur needs, and the shipped files
//! set it (a test checks).
//!
//! Option D and the mask tier: a preset is offered from [`OPTION_D_THRESHOLD_PX_PER_TRIAD`] (2.8 px per
//! triad), but `mask_tier = auto` prefilters the mask below 3 px per triad (tier C). A preset between the two
//! thresholds therefore sets `mask_tier = integrated`, and [`OptionDSuggestion::needs_integrated`] and
//! [`Preset::host_note`] tell it.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use crt_model::{
    beam::gaussian_amp_sigma_ns,
    mask::{option_d, tier, Tier, OPTION_D_THRESHOLD_PX_PER_TRIAD, TIER_B_MIN_PX_PER_TRIAD},
};

use crate::{
    params::{meta, Confidence, ParamType, ParamValue, Params, MASK_TIER_INTEGRATED, MASK_TYPE_DELTA},
    settings::{from_value, to_value},
};

/// Preset file schema version.
pub const SCHEMA_VERSION: u32 = 1;

/// Shown for a preset whose mask the host cannot resolve (PLAN P-1).
pub const NOT_RESOLVABLE_NOTE: &str = "mask not resolvable on this host";

/// Shown for a preset the host resolves only between [`OPTION_D_THRESHOLD_PX_PER_TRIAD`] and the tier B
/// threshold (3 px per triad) and whose `mask_tier` is `integrated`: the mask is drawn, without prefilter.
pub const INTEGRATED_NOTE: &str = "mask drawn with the integrated tier (ripple possible)";

/// Edge sigma (source px) at or below which the amp stage is an exact hold and extra samples change nothing
/// (`SIGMA_IDEAL` of `shaders/amp.comp`).
pub const AMP_SIGMA_IDEAL_PX: f64 = 0.01;

/// Edge sigma (source px) from which K = 16 is used. The T-33 follow-up measured the over-dimming of isolated
/// vertical lines at K = 4 against K = 16 (under 0.4 points everywhere): 2.3 / 2.6 / 2.0 / 1.1 points at
/// 5 / 8 / 15 / 30 MHz, that is at sigma 0.142 / 0.089 / 0.047 / 0.024 px with the 5.37 MHz clock. Every
/// measured point needs K = 16, and the error falls roughly linearly to the ideal limit (about 0.5 points at
/// 0.01 px), so 0.02 px, between the ideal limit and the smallest measured sigma, is where K = 16 starts.
pub const AMP_SIGMA_K16_PX: f64 = 0.02;

/// The amplifier edge sigma in source pixels that the amp stage uses for the slower edge:
/// `0.1325 x pixel clock / bandwidth` (`crt_model::beam::gaussian_amp_sigma_ns`), times the fall/rise ratio
/// when above 1 (peaking is not counted).
pub fn amp_sigma_px(params: &Params) -> f64 {
    let rise =
        gaussian_amp_sigma_ns(f64::from(params.amp_bandwidth_mhz)) * 1e-3 * f64::from(params.amp_pixel_clock_mhz);
    rise * f64::from(params.amp_fall_rise_ratio).max(1.0)
}

/// Amplifier sub-samples per native pixel that `params` needs, from the blur in source pixels
/// ([`amp_sigma_px`]): 4 (the pipeline default) when the amplifier is ideal (at most
/// [`AMP_SIGMA_IDEAL_PX`]), 16 from [`AMP_SIGMA_K16_PX`], 8 in the unmeasured gap between. The result is the
/// `amp_samples` value of a shipped preset (computed for the table's default pixel clock; a source with another
/// clock needs it recomputed).
pub fn recommended_amp_k(params: &Params) -> i32 {
    let sigma = amp_sigma_px(params);
    if sigma <= AMP_SIGMA_IDEAL_PX {
        crate::gpu::AMP_K as i32
    } else if sigma < AMP_SIGMA_K16_PX {
        8
    } else {
        16
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

/// A value as the preset files write it: enums by value name, floats in their shortest decimal form (so a
/// saved 0.31 reads back as the same f32 and the file shows 0.31, not 0.3100000023841858).
fn file_value(id: &str, v: ParamValue) -> toml::Value {
    use toml::Value as V;
    let float = |x: f32| V::Float(x.to_string().parse().unwrap_or(f64::from(x)));
    let arr = |a: &[f32]| V::Array(a.iter().map(|x| float(*x)).collect());
    match v {
        ParamValue::F32(x) => float(x),
        ParamValue::Enum(i) => match meta(id).and_then(|m| m.values.get(i as usize)) {
            Some(name) => V::String((*name).to_string()),
            None => V::Integer(i64::from(i)),
        },
        ParamValue::Vec2(a) => arr(&a),
        ParamValue::Vec3(a) => arr(&a),
        ParamValue::Vec4(a) => arr(&a),
        other => to_value(other),
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

    /// A user preset holding `params`: every id whose value differs from the table default, each with the
    /// provenance `E: user` (the source of a value the user set; the tag is an estimate by definition).
    pub fn from_params(id: &str, name: &str, params: &Params) -> Result<Preset, PresetError> {
        let file = format!("{id}.toml");
        if !is_valid_id(id) {
            return Err(err(
                &file,
                format!("id '{id}' must use lower case letters, digits and '-'"),
            ));
        }
        if name.trim().is_empty() {
            return Err(err(&file, "the name must not be empty"));
        }
        let mut values = Vec::new();
        let mut provenance = BTreeMap::new();
        for m in crate::params::PARAMS_META {
            if let Some(v) = params.get(m.id).filter(|v| *v != m.default) {
                values.push((m.id, v));
                provenance.insert(
                    m.id.to_string(),
                    Provenance {
                        tag: Confidence::Estimate,
                        source: "user".to_string(),
                    },
                );
            }
        }
        Ok(Preset {
            id: id.to_string(),
            name: name.to_string(),
            label: "saved from the live parameters (user preset)".to_string(),
            notes: "Values that differ from the table defaults, saved by the user.".to_string(),
            file,
            origin: Origin::User(PathBuf::new()),
            values,
            provenance,
            warnings: Vec::new(),
        })
    }

    /// The preset file text, in the schema of the shipped files (`Preset::parse` reads it back).
    pub fn to_toml(&self) -> String {
        let mut t = toml::Table::new();
        t.insert("schema".into(), toml::Value::Integer(i64::from(SCHEMA_VERSION)));
        t.insert("id".into(), toml::Value::String(self.id.clone()));
        t.insert("name".into(), toml::Value::String(self.name.clone()));
        t.insert("label".into(), toml::Value::String(self.label.clone()));
        if !self.notes.is_empty() {
            t.insert("notes".into(), toml::Value::String(self.notes.clone()));
        }
        let mut params = toml::Table::new();
        for (id, v) in &self.values {
            params.insert((*id).to_string(), file_value(id, *v));
        }
        let mut prov = toml::Table::new();
        for (id, p) in &self.provenance {
            prov.insert(
                id.clone(),
                toml::Value::String(format!("{}: {}", p.tag.tag(), p.source)),
            );
        }
        t.insert("params".into(), toml::Value::Table(params));
        t.insert("provenance".into(), toml::Value::Table(prov));
        toml::to_string(&t).expect("a table of strings, numbers and arrays serialises")
    }

    /// Writes the preset to `dir\<id>.toml` (atomically: temporary file, then rename) and returns the path.
    /// `dir` is created when missing; it is normally [`user_dir`].
    pub fn save(&self, dir: &Path) -> crate::Res<PathBuf> {
        fs::create_dir_all(dir)?;
        let path = dir.join(format!("{}.toml", self.id));
        let tmp = dir.join(format!("{}.toml.tmp", self.id));
        fs::write(&tmp, self.to_toml())?;
        fs::rename(&tmp, &path)?;
        Ok(path)
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

    /// True when the host gives this preset at least [`OPTION_D_THRESHOLD_PX_PER_TRIAD`] but fewer than the
    /// tier B threshold (3) px per triad: option D offers it, but `mask_tier = auto` would still prefilter
    /// the mask (tier C), so it needs the integrated tier.
    pub fn needs_integrated_tier(&self, host: (u32, u32)) -> bool {
        let ppt = self.px_per_triad(host);
        (OPTION_D_THRESHOLD_PX_PER_TRIAD..TIER_B_MIN_PX_PER_TRIAD).contains(&ppt)
    }

    /// The label of the preset on `host` (PLAN P-1), or `None` when the mask is resolved:
    /// - [`NOT_RESOLVABLE_NOTE`] below [`OPTION_D_THRESHOLD_PX_PER_TRIAD`], and also between that and 3 px per
    ///   triad when the preset's `mask_tier` is not `integrated` (the shader then averages the mask away);
    /// - [`INTEGRATED_NOTE`] between the two thresholds when `mask_tier` is `integrated`.
    pub fn host_note(&self, host: (u32, u32)) -> Option<&'static str> {
        if self.px_per_triad(host) < OPTION_D_THRESHOLD_PX_PER_TRIAD {
            Some(NOT_RESOLVABLE_NOTE)
        } else if self.needs_integrated_tier(host) {
            Some(if self.params().mask_tier == MASK_TIER_INTEGRATED {
                INTEGRATED_NOTE
            } else {
                NOT_RESOLVABLE_NOTE
            })
        } else {
            None
        }
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
    option_d_detail(presets, host).iter().map(|s| s.index).collect()
}

/// One option D suggestion: the preset (index in the list) and whether it needs `mask_tier = integrated`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OptionDSuggestion {
    pub index: usize,
    /// 2.8 <= px per triad < 3 on the host: `mask_tier = auto` would prefilter this mask (tier C), so the
    /// mask is drawn only with the integrated tier (ripple possible). The shipped presets in that range set
    /// it; for any other, the UI must set `mask_tier` to `integrated` when applying it.
    pub needs_integrated: bool,
}

/// [`option_d_suggestions`] with, for each suggestion, whether it needs the integrated mask tier.
pub fn option_d_detail(presets: &[Preset], host: (u32, u32)) -> Vec<OptionDSuggestion> {
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
        .map(|(index, p)| OptionDSuggestion {
            index,
            needs_integrated: p.needs_integrated_tier(host),
        })
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
                    Some(slot) => {
                        // A user file replacing a shipped preset is the documented way to override it; two
                        // user files with one id is a mistake the later (by file name) one wins silently.
                        if matches!(slot.origin, Origin::User(_)) {
                            self.warnings.push(format!(
                                "preset {}: id '{}' is already defined by {}; this file replaces it",
                                p.file, p.id, slot.file
                            ));
                        }
                        *slot = p;
                    }
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

    /// The suggestions of [`Library::option_d`] with whether each needs the integrated mask tier.
    pub fn option_d_detail(&self, host: (u32, u32)) -> Vec<OptionDSuggestion> {
        option_d_detail(&self.presets, host)
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
            // Unmasked white within panel white: the linear sRGB maximum of the tube white at Y = 1, times
            // exposure, is <= 1. This is the white balance limit only: the mask and scanline peaks lift elements
            // above it, handled by the headroom mode (T-84, D-16, T-84b: every preset inherits the auto default; the flat-white
            // chain test of tests/presets.rs measures what each mode keeps).
            let w = xy_to_srgb_max(q.tube_white_xy);
            assert!(w * f64::from(q.exposure) <= 1.0 + 1e-3, "{}: white clips ({w})", p.id);
            assert_eq!(
                q.output_headroom_mode,
                crate::params::OUTPUT_HEADROOM_MODE_AUTO,
                "{}: D-16",
                p.id
            );
            // Slow-rise direction on consumer amplifiers (r < 1 dims isolated pixels), symmetric elsewhere.
            assert!(q.amp_fall_rise_ratio <= 1.0, "{}", p.id);
        }
        let l = lib();
        assert_eq!(l.find("pvm-20m4u").unwrap().params().mask_damper_count, 2);
        assert_eq!(l.find("bvm-20f1u").unwrap().params().mask_damper_count, 2);
        assert_eq!(l.find("kv-trinitron-tv").unwrap().params().exposure, 0.81);
    }

    /// Linear sRGB of the light of XYZ (Rec.709 matrix).
    fn xyz_to_srgb(xyz: [f64; 3]) -> [f64; 3] {
        let [x, y, z] = xyz;
        [
            3.2406 * x - 1.5372 * y - 0.4986 * z,
            -0.9689 * x + 1.8758 * y + 0.0415 * z,
            0.0557 * x - 0.2040 * y + 1.0570 * z,
        ]
    }

    /// Largest linear sRGB channel of each pure tube primary at full drive (white at Y = 1 built from the
    /// primaries and the white point), times the exposure.
    fn primary_maxima(q: &Params) -> [f64; 3] {
        let col = |xy: [f32; 2]| {
            let (x, y) = (f64::from(xy[0]), f64::from(xy[1]));
            [x / y, 1.0, (1.0 - x - y) / y]
        };
        let cols = [col(q.tube_red_xy), col(q.tube_green_xy), col(q.tube_blue_xy)];
        let w = col(q.tube_white_xy);
        let det = |m: &[[f64; 3]; 3]| {
            m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
                + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
        };
        // Rows of the matrix are the XYZ components, columns the primaries; Cramer's rule for the scales.
        let m = |i: usize, j: usize| cols[j][i];
        let a = [
            [m(0, 0), m(0, 1), m(0, 2)],
            [m(1, 0), m(1, 1), m(1, 2)],
            [m(2, 0), m(2, 1), m(2, 2)],
        ];
        let d = det(&a);
        let mut out = [0.0; 3];
        for (k, o) in out.iter_mut().enumerate() {
            let mut b = a;
            for (row, wi) in b.iter_mut().zip(w) {
                row[k] = wi;
            }
            let s = det(&b) / d;
            let xyz = [cols[k][0] * s, cols[k][1] * s, cols[k][2] * s];
            let rgb = xyz_to_srgb(xyz);
            *o = rgb.iter().copied().fold(f64::MIN, f64::max) * f64::from(q.exposure);
        }
        out
    }

    /// Informational: the pure tube primaries that clip in linear sRGB at the preset exposure (the no-clip check
    /// covers white only). Prints the list; fails only if white itself were wrongly computed.
    #[test]
    fn clipped_primaries_are_reported_per_preset() {
        for p in lib().list() {
            let q = p.params();
            let m = primary_maxima(&q);
            let clipped: Vec<String> = ["red", "green", "blue"]
                .iter()
                .zip(m)
                .filter(|(_, v)| *v > 1.0 + 1e-3)
                .map(|(n, v)| format!("{n} {v:.3}"))
                .collect();
            eprintln!(
                "clipped primaries (linear sRGB, exposure {}): {:<16} {}",
                q.exposure,
                p.id,
                if clipped.is_empty() {
                    "none".into()
                } else {
                    clipped.join(", ")
                }
            );
            assert!(m.iter().all(|v| v.is_finite() && *v > 0.0), "{}", p.id);
        }
    }

    /// Preset 3: 9300 K + 27 MPCD (CRT.md, Japanese TV practice, not D93) with the exposure derived from its
    /// linear sRGB blue.
    #[test]
    fn kv_trinitron_white_is_9300k_plus_27_mpcd_with_exposure_below_0_811() {
        let q = lib().find("kv-trinitron-tv").unwrap().params();
        assert_eq!(q.tube_white_xy, [0.281, 0.311]);
        let blue = xy_to_srgb_max([0.281, 0.311]);
        assert!(
            (blue - 1.233).abs() < 1e-3 && (1.0 / blue - 0.811).abs() < 1e-3,
            "{blue}"
        );
        assert!(q.exposure <= 0.811 && f64::from(q.exposure) * blue <= 1.0 + 1e-3);
    }

    /// T-36b review: presets 1 and 2 use n = 0.5, which gives a peak to floor ratio of about 2.5.
    #[test]
    fn pro_monitor_spot_growth_has_a_peak_to_floor_ratio_near_2_5() {
        for id in ["pvm-20m4u", "bvm-20f1u"] {
            let q = lib().find(id).unwrap().params();
            assert_eq!(q.beam_growth_exponent, 0.5, "{id}");
            let g = SpotGrowth {
                fwhm_mid: f64::from(q.beam_fwhm_mid_mm),
                fwhm_peak: f64::from(q.beam_fwhm_peak_mm),
                n: f64::from(q.beam_growth_exponent),
            };
            let d0 = g.floor_sq_and_k().0.sqrt();
            let ratio = g.fwhm_peak / d0;
            assert!((2.0..3.0).contains(&ratio), "{id}: peak/d0 {ratio}");
        }
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
    fn amp_samples_of_every_preset_is_the_recommended_k() {
        let l = lib();
        let want = [16, 16, 16, 4, 16, 16, 4, 16, 4, 4];
        for (p, w) in l.list().iter().zip(want) {
            let q = p.params();
            assert_eq!(q.amp_samples, p.recommended_amp_k(), "{}", p.id);
            assert_eq!(q.amp_samples, w, "{}: sigma {} px", p.id, amp_sigma_px(&q));
            assert!(p.provenance.contains_key("amp_samples"), "{}", p.id);
        }
        // The default (100 MHz, 5.37 MHz clock) is an ideal amplifier: K = 4, which is also the default value.
        assert_eq!(recommended_amp_k(&Params::default()), 4);
        assert_eq!(Params::default().amp_samples, crate::gpu::AMP_K as i32);
    }

    #[test]
    fn recommended_k_follows_the_blur_in_source_pixels() {
        let k = |bw: f32, clock: f32, ratio: f32| {
            let mut p = Params::default();
            assert!(p.set("amp_bandwidth_mhz", ParamValue::F32(bw)));
            assert!(p.set("amp_pixel_clock_mhz", ParamValue::F32(clock)));
            assert!(p.set("amp_fall_rise_ratio", ParamValue::F32(ratio)));
            (amp_sigma_px(&p), recommended_amp_k(&p))
        };
        // The T-33 follow-up points (5.37 MHz clock): sigma 0.142 / 0.089 / 0.047 / 0.024 px all need K = 16.
        for bw in [5.0, 8.0, 15.0, 30.0] {
            assert_eq!(k(bw, 5.37, 1.0).1, 16, "{bw} MHz");
        }
        let (s, kk) = k(100.0, 5.37, 1.0);
        assert!((s - 0.1325 * 5.37 / 100.0).abs() < 1e-4 && kk == 4);
        // The pixel clock matters: the same 100 MHz amplifier on a 25 MHz VGA clock blurs 0.033 px.
        assert_eq!(k(100.0, 25.175, 1.0).1, 16);
        // The gap between the ideal limit and the smallest measured sigma.
        let (s, kk) = k(50.0, 5.37, 1.0);
        assert!(s > AMP_SIGMA_IDEAL_PX && s < AMP_SIGMA_K16_PX && kk == 8, "{s}");
        // A slow fall counts (the stage uses the slower edge); a slow rise does not.
        assert_eq!(k(80.0, 5.37, 1.0).1, 4);
        assert_eq!(k(80.0, 5.37, 2.0).1, 8);
        assert_eq!(k(80.0, 5.37, 0.5).1, 4);
        assert_eq!(k(60.0, 5.37, 2.0).1, 16);
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
        // Presets 3 (2.96) and 5 (2.98) are offered by option D but tier C by the shader's 3 px threshold:
        // they set mask_tier = integrated and say so.
        for i in [2, 4] {
            let p = &l.list()[i];
            assert!(p.px_per_triad(HOST) > OPTION_D_THRESHOLD_PX_PER_TRIAD && p.px_per_triad(HOST) < 3.0);
            assert_eq!(p.params().mask_tier, MASK_TIER_INTEGRATED, "{}", p.id);
            assert_eq!(p.host_note(HOST), Some(INTEGRATED_NOTE), "{}", p.id);
        }
        // Everything else has the automatic tier, and no other preset carries the integrated note.
        for (i, p) in l.list().iter().enumerate() {
            if ![2, 4].contains(&i) {
                assert_ne!(p.params().mask_tier, MASK_TIER_INTEGRATED, "{}", p.id);
                assert_ne!(p.host_note(HOST), Some(INTEGRATED_NOTE), "{}", p.id);
            }
        }
    }

    #[test]
    fn option_d_reports_which_suggestions_need_the_integrated_tier() {
        let l = lib();
        let d = l.option_d_detail(HOST);
        let flags: Vec<(usize, bool)> = d.iter().map(|s| (s.index + 1, s.needs_integrated)).collect();
        assert_eq!(flags, vec![(3, true), (5, true), (6, false), (8, false)]);
        // The same list as the plain suggestions.
        assert_eq!(l.option_d(HOST), d.iter().map(|s| s.index).collect::<Vec<_>>());
        // Every suggestion that needs the integrated tier sets it (else its mask would be averaged away),
        // and the note of a preset with the automatic tier in that range stays "not resolvable".
        for s in &d {
            let p = &l.list()[s.index];
            assert_eq!(s.needs_integrated, p.needs_integrated_tier(HOST));
            if s.needs_integrated {
                assert_eq!(p.params().mask_tier, MASK_TIER_INTEGRATED, "{}", p.id);
            }
        }
        let mut auto = l.list()[2].clone();
        auto.values.retain(|(id, _)| *id != "mask_tier");
        assert_eq!(auto.host_note(HOST), Some(NOT_RESOLVABLE_NOTE));
        // A 4K host resolves from 3 px per triad: nothing needs the integrated tier there.
        assert!(l.option_d_detail((3840, 2160)).iter().all(|s| !s.needs_integrated));
    }

    #[test]
    fn saved_preset_round_trips_to_identical_params() {
        let l = lib();
        let dir = std::env::temp_dir().join(format!("ucrt-presets-save-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        // Every shipped preset, with edits of every value kind on top.
        for base in l.list() {
            let mut cur = base.params();
            assert!(cur.set("exposure", ParamValue::F32(0.6123)));
            assert!(cur.set("gun_white_balance", ParamValue::Bool(true)));
            assert!(cur.set("tube_red_xy", ParamValue::Vec2([0.64, 0.33])));
            assert!(cur.set("line_multiplier", ParamValue::I32(2)));
            assert!(cur.set("mask_tier", ParamValue::Enum(2)));
            let saved = Preset::from_params(&format!("my-{}", base.id), "My preset", &cur).unwrap();
            assert!(saved
                .provenance
                .values()
                .all(|p| p.source == "user" && p.tag == Confidence::Estimate));
            let path = saved.save(&dir).unwrap();
            let text = fs::read_to_string(&path).unwrap();
            let back = Preset::parse(&path.display().to_string(), &text, Origin::User(path.clone()))
                .unwrap_or_else(|e| panic!("{e}\n{text}"));
            assert_eq!(back.params(), cur, "{}\n{text}", base.id);
            assert!(back.warnings.is_empty(), "{:?}", back.warnings);
            assert_eq!((back.id.as_str(), back.name.as_str()), (saved.id.as_str(), "My preset"));
            // The file shows plain decimals, enums by name, and the schema of the shipped files.
            assert!(
                text.contains("schema = 1") && text.contains("mask_tier = \"prefiltered\""),
                "{text}"
            );
            assert!(text.contains("exposure = 0.6123"), "{text}");
            assert!(text.contains("E: user"), "{text}");
        }
        // The saved files load as user presets through the library, next to the shipped ones.
        let lib2 = Library::load(Some(&dir));
        assert!(lib2.warnings.is_empty(), "{:?}", lib2.warnings);
        assert_eq!(lib2.list().len(), 20);
        // A shipped preset written back unchanged also round-trips.
        let kv = l.find("kv-trinitron-tv").unwrap();
        let again = Preset::parse("kv.toml", &kv.to_toml(), Origin::Embedded).unwrap();
        assert_eq!(again.params(), kv.params());
        assert_eq!(again.provenance, kv.provenance);
        // Invalid ids are refused.
        assert!(Preset::from_params("Bad Id", "n", &Params::default()).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_user_files_with_one_id_warn() {
        let dir = std::env::temp_dir().join(format!("ucrt-presets-dup-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.toml"), sample("")).unwrap();
        fs::write(
            dir.join("b.toml"),
            sample("").replace("exposure = 0.5", "exposure = 0.9"),
        )
        .unwrap();
        let l = Library::load(Some(&dir));
        assert_eq!(l.list().len(), 11);
        assert_eq!(l.find("x").unwrap().params().exposure, 0.9, "the later file wins");
        assert_eq!(l.warnings.len(), 1, "{:?}", l.warnings);
        assert!(
            l.warnings[0].contains("b.toml") && l.warnings[0].contains("a.toml") && l.warnings[0].contains("'x'"),
            "{:?}",
            l.warnings
        );
        let _ = fs::remove_dir_all(&dir);
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
    /// Viewable diagonal of the preset tube (mm).
    fn diagonal_mm(q: &Params) -> f64 {
        let h = f64::from(q.tube_visible_height_mm);
        h.hypot(h * f64::from(q.tube_aspect))
    }

    /// T-64: the faceplate radius of each preset gives the CRT.md corner sagitta of its tube class, checked with
    /// the `crt_model::geometry` oracle: spherical 1R (R = 1.767 diagonals) 7.2 % of the diagonal, 1.5R 4.8 %;
    /// the cylindrical tubes lie in the patent range 1.33 to 1.75 x 1R (Trinitron) or the flat-face inner radii
    /// of CRT.md (2300 to 2500 mm).
    #[test]
    fn faceplate_radius_gives_the_crt_md_sagitta() {
        use crt_model::geometry::sagitta_mm;
        let l = lib();
        // (id, R / diagonal, corner sagitta / diagonal): CRT.md Faceplate curvature table.
        let spherical = [
            ("slot-tv-26in", 1.767, 0.072),
            ("tv-13in", 1.767, 0.072),
            ("arcade-19in", 1.767, 0.072),
            ("cromaclear-17in", 1.5 * 1.767, 0.048),
            ("vga-15in", 1.5 * 1.767, 0.048),
            ("delta-17in", 1.5 * 1.767, 0.048),
        ];
        for (id, ratio, sag) in spherical {
            let q = l.find(id).unwrap().params();
            assert_eq!(
                q.geometry_curvature,
                crate::params::GEOMETRY_CURVATURE_SPHERICAL,
                "{id}"
            );
            let (d, r) = (diagonal_mm(&q), f64::from(q.geometry_radius_mm));
            assert!((r / d - ratio).abs() < 0.002, "{id}: R/D {}", r / d);
            let s = sagitta_mm(r, d / 2.0) / d;
            assert!((s - sag).abs() < 0.0006, "{id}: corner sagitta {s} of the diagonal");
        }
        // Hand check, 26 in: R 1166 mm, corner sagitta about 0.072 x 660 = 47.6 mm.
        let q = l.find("slot-tv-26in").unwrap().params();
        assert!((sagitta_mm(f64::from(q.geometry_radius_mm), 330.0) - 47.6).abs() < 0.3);
        for id in ["pvm-20m4u", "bvm-20f1u", "kv-trinitron-tv"] {
            let q = l.find(id).unwrap().params();
            assert_eq!(
                q.geometry_curvature,
                crate::params::GEOMETRY_CURVATURE_CYLINDRICAL,
                "{id}"
            );
            let (d, r) = (diagonal_mm(&q), f64::from(q.geometry_radius_mm));
            let ratio = r / (1.767 * d);
            assert!(
                (1.33..=1.75).contains(&ratio),
                "{id}: {ratio} x 1R outside the patent range"
            );
            // Horizontal edge sagitta stays below the 1R corner value 7.2 % of the diagonal.
            let w = f64::from(q.tube_visible_height_mm) * f64::from(q.tube_aspect);
            assert!(sagitta_mm(r, w / 2.0) / d < 0.072, "{id}");
        }
        let q = l.find("kv-trinitron-tv").unwrap().params();
        assert!((1594.0..=2100.0).contains(&f64::from(q.geometry_radius_mm)));
        let q = l.find("gdm-f520").unwrap().params();
        assert_eq!(q.geometry_curvature, crate::params::GEOMETRY_CURVATURE_CYLINDRICAL);
        assert!((2300.0..=2500.0).contains(&f64::from(q.geometry_radius_mm)));
    }

    // T-87 (audit m5): the confidence tags must be honest and the loader data must enforce it.

    const WEAK_WORDS: [&str; 6] = ["estimate", "borrowed", "weak", "sibling", "fitted", "chosen"];

    fn strength(c: Confidence) -> u8 {
        match c {
            Confidence::Sourced => 3,
            Confidence::Derived | Confidence::Known => 2,
            Confidence::Estimate => 1,
        }
    }

    /// A recognisable reference: patent number, URL or DOI, author and year, a standard id, or CRT.md.
    fn has_reference(s: &str) -> bool {
        let b = s.as_bytes();
        let digits_after = |p: usize| b.get(p).is_some_and(|c| c.is_ascii_digit());
        let patent = s.match_indices("US ").any(|(i, _)| digits_after(i + 3));
        let author_year = s.match_indices(|c: char| c.is_ascii_digit()).any(|(i, _)| {
            i >= 2
                && s.is_char_boundary(i + 4)
                && s[i..i + 4].chars().all(|c| c.is_ascii_digit())
                && (s[i - 1..i] == *" " || s[i - 1..i] == *"(")
                && s[..i]
                    .trim_end_matches([' ', '('])
                    .chars()
                    .last()
                    .is_some_and(|c| c.is_alphabetic())
                && ["19", "20"].contains(&&s[i..i + 2])
        });
        patent
            || author_year
            || [
                "http://", "https://", "doi", "BT.1886", "BT.709", "BT.601", "SMPTE", "EBU", "ITU-R", "CRT.md",
            ]
            .iter()
            .any(|k| s.contains(k))
    }

    fn check_entry(what: &str, tag: Confidence, source: &str, bad: &mut Vec<String>) {
        if source.trim().is_empty() {
            bad.push(format!("{what}: empty source"));
        }
        if matches!(tag, Confidence::Sourced | Confidence::Derived) {
            let low = source.to_lowercase();
            if let Some(w) = WEAK_WORDS.iter().find(|w| low.contains(**w)) {
                bad.push(format!("{what}: {} with the word '{w}' in its source", tag.tag()));
            }
        }
        if tag == Confidence::Sourced && !has_reference(source) {
            bad.push(format!("{what}: S without a recognisable reference"));
        }
    }

    #[test]
    fn parameter_tags_are_honest() {
        let mut bad = Vec::new();
        for m in PARAMS_META.iter().filter(|m| m.stage != "_test") {
            check_entry(&format!("param {}", m.id), m.confidence, m.source, &mut bad);
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    #[test]
    fn preset_tags_are_honest_and_not_stronger_than_the_parameter_without_a_new_source() {
        let mut bad = Vec::new();
        for p in lib().list() {
            for (id, pv) in &p.provenance {
                check_entry(&format!("preset {} {id}", p.id), pv.tag, &pv.source, &mut bad);
                let m = meta(id).unwrap();
                if strength(pv.tag) > strength(m.confidence) && pv.source == m.source {
                    bad.push(format!(
                        "preset {} {id}: stronger than the parameter with the same source",
                        p.id
                    ));
                }
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }
    /// T-64: persistence is on for every preset with the Kuhn P22 constants (tail strength 1), and the 60 Hz frame
    /// energy ratios of the oracle are the CRT.md table (green 5.36 %, blue 4.72 %); ABL is on exactly for the
    /// consumer TV presets with the sourced RCA 1.4 mA threshold; raster breathing and saturation keep their
    /// defaults; mask_tier_blend stays the parameter default (D-15).
    #[test]
    fn m4_values_follow_the_recorded_policies() {
        use crt_model::phosphor::{KUHN_BLUE, KUHN_GREEN};
        let t = crt_model::units::frame_period_ms(60.0);
        assert!((KUHN_GREEN.frame_energy_ratios(t)[0] * 100.0 - 5.36).abs() < 0.01);
        assert!((KUHN_BLUE.frame_energy_ratios(t)[0] * 100.0 - 4.72).abs() < 0.01);
        let d = Params::default();
        for p in lib().list() {
            let q = p.params();
            assert!(q.phosphor_persistence, "{}", p.id);
            assert_eq!(q.phosphor_tail_strength_rgb, [1.0; 3], "{}", p.id);
            let tv = ["kv-trinitron-tv", "slot-tv-26in", "tv-13in"].contains(&p.id.as_str());
            assert_eq!(q.abl_enable, tv, "{}", p.id);
            if tv {
                assert_eq!(q.abl_threshold_ma, 1.4, "{}", p.id);
            }
            assert_eq!(q.abl_breathing, d.abl_breathing, "{}", p.id);
            assert_eq!(q.phosphor_saturation, d.phosphor_saturation, "{}", p.id);
            assert_eq!(q.mask_tier_blend, d.mask_tier_blend, "{}", p.id);
            assert_ne!(q.geometry_curvature, crate::params::GEOMETRY_CURVATURE_FLAT, "{}", p.id);
            for k in ["geometry_curvature", "geometry_radius_mm", "phosphor_persistence"] {
                assert!(
                    p.values.iter().any(|(id, _)| *id == k) && p.provenance.contains_key(k),
                    "{}: {k}",
                    p.id
                );
            }
        }
    }
}
