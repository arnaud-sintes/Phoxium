//! CRT parameters, generated from the declarative table in `params/*.toml` (TEC-3, DESIGN section 8).
//!
//! `build.rs` runs `crates/paramgen`, which writes `params.rs` (included here) and `params.glsl` (the
//! matching std140 uniform block, see [`PARAMS_GLSL`]) to `OUT_DIR`. Generated items:
//! - [`Params`]: one field per parameter, `Default` gives the table defaults;
//! - [`PARAMS_META`]: id, stage, type, unit, default, soft range, confidence tag, source, description,
//!   UI group, enum values, defining file and std140 offset of every parameter, in layout order;
//! - [`Params::write_std140`] and [`Params::to_std140`]: the uniform buffer bytes ([`PARAMS_STD140_SIZE`]);
//! - [`Params::get`] and [`Params::set_checked`]: access by id for the UI and the presets;
//! - one `u32` constant per enum value, `<ID>_<VALUE>`, also defined in GLSL.
//!
//! Test-only entries (`params/_test.toml`) exist only in non-release builds, under `cfg(phoxium_test_params)`.
//!
//! # Validation (R-05)
//!
//! Every input path (presets, settings overrides, `render --set`, the settings menu) writes parameters through
//! [`Params::set_checked`] or its wrapper [`Params::set`], the one validation point:
//! - a non-finite float (NaN, infinity, in any vector component) is rejected and the old value kept;
//! - an enum index outside its value list is rejected; an unknown id or a value of the wrong type too;
//! - a number outside the **hard range** ([`ParamMeta::hard_range`], declared in `params/*.toml`, with its
//!   reason in [`ParamMeta::hard_why`]) is clamped into it, per component;
//! - then the cross-parameter rules of [`Params::constrain`] apply (see there).
//!
//! The soft range is the slider range: the menu clamps a typed value to it before calling `set`
//! (`ui::panels::clamp_to_slider`); files may go beyond it, up to the hard range. [`Params::set`] records
//! every clamp and every non-finite rejection as a notice ([`notices`]) that the settings menu shows in its
//! Performance panel; callers that report with their own context (a file name and key) use `set_checked` and
//! [`clamp_message`].

/// Type of a parameter. Enum values are `u32` indices into [`ParamMeta::values`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamType {
    F32,
    I32,
    Bool,
    Enum,
    Vec2,
    Vec3,
    Vec4,
}

/// A parameter value, typed like the [`Params`] field it belongs to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ParamValue {
    F32(f32),
    I32(i32),
    Bool(bool),
    Enum(u32),
    Vec2([f32; 2]),
    Vec3([f32; 3]),
    Vec4([f32; 4]),
}

/// Confidence of a value (`CRT.md` tags).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    /// `[S]` sourced.
    Sourced,
    /// `[D]` derived.
    Derived,
    /// `[K]` standard knowledge.
    Known,
    /// `[E]` estimate.
    Estimate,
}

impl Confidence {
    /// The one-letter tag used in `CRT.md` and in the parameter files.
    pub fn tag(self) -> char {
        match self {
            Confidence::Sourced => 'S',
            Confidence::Derived => 'D',
            Confidence::Known => 'K',
            Confidence::Estimate => 'E',
        }
    }
}

/// The settings menu widget of a parameter (`widget` in `params/*.toml`, validated by `paramgen`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamWidget {
    /// From the type: a slider (logarithmic when the soft range is positive and spans two decades or more), a
    /// check box, a combo.
    Auto,
    /// A linear slider.
    Linear,
    /// A logarithmic slider (positive soft range).
    Log,
    /// A colour picker (vec3 within 0 to 1).
    Color,
}

/// Metadata of one parameter.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParamMeta {
    pub id: &'static str,
    /// Pipeline stage, equal to the stem of the defining file.
    pub stage: &'static str,
    pub ty: ParamType,
    /// Physical unit, `"1"` for dimensionless.
    pub unit: &'static str,
    pub default: ParamValue,
    /// Soft range (the slider range; a typed value is clamped to it, a file value outside it is kept with a
    /// warning), per component for vectors. `None` for bool and enum.
    pub soft_range: Option<(f64, f64)>,
    /// Hard validity range, per component for vectors, containing the soft range: [`Params::set_checked`]
    /// clamps every value into it. `None` for bool and enum.
    pub hard_range: Option<(f64, f64)>,
    /// Why values beyond the hard range are invalid (`hard_why` in the file), empty when not given.
    pub hard_why: &'static str,
    pub confidence: Confidence,
    pub source: &'static str,
    pub description: &'static str,
    /// UI group.
    pub group: &'static str,
    /// Enum choices, empty for other types.
    pub values: &'static [&'static str],
    /// Shown in the settings menu only with "Advanced" on (or when a search matches it).
    pub advanced: bool,
    /// The settings menu widget (`widget` in the file, [`ParamWidget::Auto`] when absent).
    pub widget: ParamWidget,
    /// Defining file, such as `params/output.toml`.
    pub file: &'static str,
    /// Byte offset of the member in the std140 block.
    pub offset: usize,
    /// Size in bytes of the member in the std140 block.
    pub size: usize,
}

/// Metadata of the parameter `id`.
pub fn meta(id: &str) -> Option<&'static ParamMeta> {
    PARAMS_META.iter().find(|m| m.id == id)
}

/// An accepted [`Params::set_checked`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Applied {
    /// Stored as given.
    Exact,
    /// Clamped into the hard range or by a cross-parameter rule ([`Params::constrain`]): the value stored.
    Clamped(ParamValue),
}

/// Why [`Params::set_checked`] refused a value (nothing changed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejected {
    UnknownId,
    WrongType,
    /// NaN or an infinity (in any component).
    NonFinite,
    /// An enum index outside the value list.
    NotAValue,
}

impl std::fmt::Display for Rejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Rejected::UnknownId => "unknown parameter",
            Rejected::WrongType => "value of the wrong type",
            Rejected::NonFinite => "not a finite number",
            Rejected::NotAValue => "not one of the values",
        })
    }
}

/// Smallest non-zero `source_display_aspect` (a forced aspect, 0 = off): the hard range [0, 10] allows 0, and a
/// tiny non-zero aspect would give a tube rectangle narrower than a pixel (an empty tube domain). Equal to the
/// hard minimum of `tube_aspect`.
pub const MIN_DISPLAY_ASPECT: f32 = 0.25;

/// The cross-parameter rule that [`Params::constrain`] applies after a value of `id` is stored, in words, or
/// `None` when `id` has none.
pub fn constraint_rule(id: &str) -> Option<&'static str> {
    Some(match id {
        "optics_halation_weight" | "optics_glare_weight" | "optics_bloom_weight" => {
            "the halation, glare and glow (bloom) weights share the emitted light and must not sum above 1 (else \
             the direct picture gets a negative weight); the value just set is lowered to 1 minus the others"
        }
        "source_display_aspect" => "a forced display aspect is 0 (off) or at least 0.25",
        _ => return None,
    })
}

/// A value as messages show it: numbers in their shortest form, enums by name, vectors in brackets.
pub fn value_text(id: &str, v: ParamValue) -> String {
    let list = |a: &[f32]| format!("[{}]", a.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(", "));
    match v {
        ParamValue::F32(x) => x.to_string(),
        ParamValue::I32(x) => x.to_string(),
        ParamValue::Bool(x) => x.to_string(),
        ParamValue::Enum(i) => meta(id)
            .and_then(|m| m.values.get(i as usize))
            .map_or_else(|| i.to_string(), |s| (*s).to_string()),
        ParamValue::Vec2(a) => list(&a),
        ParamValue::Vec3(a) => list(&a),
        ParamValue::Vec4(a) => list(&a),
    }
}

/// Whether every number of `v` lies inside the hard range of `m` (true for bool and enum).
pub fn in_hard_range(m: &ParamMeta, v: ParamValue) -> bool {
    let Some((lo, hi)) = m.hard_range else {
        return true;
    };
    let inside = |x: f32| x.is_finite() && (lo as f32..=hi as f32).contains(&x);
    match v {
        ParamValue::F32(x) => inside(x),
        ParamValue::I32(x) => (lo..=hi).contains(&f64::from(x)),
        ParamValue::Vec2(a) => a.iter().all(|x| inside(*x)),
        ParamValue::Vec3(a) => a.iter().all(|x| inside(*x)),
        ParamValue::Vec4(a) => a.iter().all(|x| inside(*x)),
        ParamValue::Bool(_) | ParamValue::Enum(_) => true,
    }
}

/// The message of a clamp by [`Params::set_checked`]: the parameter `id` was given `given` and holds `stored`.
/// Names the hard range and its reason, or the cross-parameter rule.
pub fn clamp_message(id: &str, given: ParamValue, stored: ParamValue) -> String {
    let (g, s) = (value_text(id, given), value_text(id, stored));
    match meta(id) {
        Some(m) if !in_hard_range(m, given) => {
            let (lo, hi) = m.hard_range.unwrap_or_default();
            let unit = if m.unit == "1" {
                String::new()
            } else {
                format!(" {}", m.unit)
            };
            let why = if m.hard_why.is_empty() {
                String::new()
            } else {
                format!(" ({})", m.hard_why)
            };
            format!("parameter '{id}' = {g} is outside its valid range {lo} to {hi}{unit}{why}; set to {s}")
        }
        _ => match constraint_rule(id) {
            Some(rule) => format!("parameter '{id}' = {g}: {rule}; set to {s}"),
            None => format!("parameter '{id}' = {g} was adjusted to {s}"),
        },
    }
}

/// Most notices kept ([`notify`]): the oldest goes first.
pub const MAX_NOTICES: usize = 8;

thread_local! {
    /// The parameter notices of this thread (the app sets and shows parameters on its main thread only).
    static NOTICES: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Records a parameter notice (a clamped or rejected value) for the settings menu. An identical notice
/// already recorded is not repeated; at most [`MAX_NOTICES`] are kept.
pub fn notify(message: String) {
    NOTICES.with(|n| {
        let mut n = n.borrow_mut();
        if !n.contains(&message) {
            if n.len() == MAX_NOTICES {
                n.remove(0);
            }
            n.push(message);
        }
    });
}

/// The parameter notices recorded on this thread, oldest first.
pub fn notices() -> Vec<String> {
    NOTICES.with(|n| n.borrow().clone())
}

/// Forgets the notices (the menu's "Dismiss").
pub fn clear_notices() {
    NOTICES.with(|n| n.borrow_mut().clear());
}

/// The generated GLSL header: enum constants and the `Params` uniform block, instance `params`, at set and
/// binding 0 unless `PHOXIUM_PARAMS_SET` and `PHOXIUM_PARAMS_BINDING` are defined before the include. Shaders
/// include it from `OUT_DIR` (`params.glsl`); this copy serves tests and diagnostics.
pub const PARAMS_GLSL: &str = include_str!(concat!(env!("OUT_DIR"), "/params.glsl"));

include!(concat!(env!("OUT_DIR"), "/params.rs"));

impl Params {
    /// The defaults with an identity gun (power law of exponent 1, which `gun.comp` returns unchanged, bit
    /// exact). With `Settings::identity()` the output codes equal the captured codes (T-34 moved the tube
    /// gamma from `decode` to the `gun` slot, so `Params::default()` no longer passes codes through).
    pub fn identity_gun() -> Self {
        Self {
            gun_gamma: 1.0,
            ..Self::default()
        }
    }

    /// [`Params::set_checked`] for callers without their own report: true when the value was stored (as given
    /// or clamped), false when it was rejected (nothing changed). A clamp and a non-finite value are recorded
    /// as a notice ([`notify`]).
    pub fn set(&mut self, id: &str, value: ParamValue) -> bool {
        match self.set_checked(id, value) {
            Ok(Applied::Exact) => true,
            Ok(Applied::Clamped(stored)) => {
                notify(clamp_message(id, value, stored));
                true
            }
            Err(Rejected::NonFinite) => {
                notify(format!(
                    "parameter '{id}' = {}: not a finite number, ignored (the value is kept)",
                    value_text(id, value)
                ));
                false
            }
            Err(_) => false,
        }
    }

    /// The cross-parameter rules, run by [`Params::set_checked`] after the value of `id` is stored (and
    /// clamped into its hard range). True when a value changed. Each rule lowers the parameter just set, so the
    /// last edit yields and earlier ones are kept (presets and overrides apply in id order, which makes the
    /// result deterministic):
    /// - **Optics energy:** `optics_halation_weight + optics_glare_weight + optics_bloom_weight <= 1`.
    ///   `optics.comp` gives the direct picture the weight `1 - halation - glare - bloom x tint x feed` (tint and
    ///   feed in [0, 1]); above 1 it would be negative (negative light, R-49). With the sum at most 1 the weights
    ///   are a partition of the emitted light, so the total light of the picture is kept (energy conserving).
    ///   `optics_diffusion_weight` is not part of the sum: the phosphor
    ///   diffusion is a separate convex blend `(1 - w) x + w blur` before the halo, conserving on its own for
    ///   any `w` in its hard range [0, 1].
    /// - **Display aspect:** `source_display_aspect` is 0 (off) or at least [`MIN_DISPLAY_ASPECT`].
    fn constrain(&mut self, id: &str) -> bool {
        fn cap(v: &mut f32, max: f32) -> bool {
            let max = max.max(0.0);
            if *v > max {
                *v = max;
                true
            } else {
                false
            }
        }
        match id {
            "optics_halation_weight" => cap(
                &mut self.optics_halation_weight,
                1.0 - self.optics_glare_weight - self.optics_bloom_weight,
            ),
            "optics_glare_weight" => cap(
                &mut self.optics_glare_weight,
                1.0 - self.optics_halation_weight - self.optics_bloom_weight,
            ),
            "optics_bloom_weight" => cap(
                &mut self.optics_bloom_weight,
                1.0 - self.optics_halation_weight - self.optics_glare_weight,
            ),
            "source_display_aspect" if self.source_display_aspect > 0.0 => {
                let low = self.source_display_aspect < MIN_DISPLAY_ASPECT;
                if low {
                    self.source_display_aspect = MIN_DISPLAY_ASPECT;
                }
                low
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_metadata() {
        let p = Params::default();
        for m in PARAMS_META {
            assert_eq!(p.get(m.id), Some(m.default), "{}", m.id);
        }
        assert_eq!(p.get("no_such_parameter"), None);
    }

    #[test]
    fn production_entries_are_present() {
        let exposure = meta("exposure").unwrap();
        assert_eq!(exposure.file, "params/output.toml");
        assert_eq!(exposure.ty, ParamType::F32);
        assert_eq!(exposure.default, ParamValue::F32(1.0));
        for id in ["tube_red_xy", "tube_green_xy", "tube_blue_xy", "tube_white_xy"] {
            let m = meta(id).unwrap();
            assert_eq!(m.file, "params/colour.toml");
            assert_eq!(m.ty, ParamType::Vec2);
        }
        assert_eq!(Params::default().tube_white_xy, [0.3127, 0.3290]);
    }

    #[test]
    fn source_and_tube_entries_have_the_t30_defaults() {
        let p = Params::default();
        assert_eq!(p.source_eotf, SOURCE_EOTF_DRIVE_VOLTAGE);
        assert_eq!(meta("source_eotf").unwrap().values, ["drive_voltage", "srgb"]);
        assert_eq!(p.raster_aspect, RASTER_ASPECT_PAR);
        assert_eq!((p.source_par, p.line_multiplier), (1.0, 1));
        assert_eq!((p.tube_visible_height_mm, p.tube_height_fraction), (290.0, 1.0));
        assert!((p.tube_aspect - 4.0 / 3.0).abs() < 1e-6);
        assert_eq!(meta("tube_aspect").unwrap().file, "params/tube.toml");
        assert_eq!(meta("line_multiplier").unwrap().file, "params/source.toml");
    }

    #[test]
    fn gun_entries_have_the_t34_defaults() {
        let p = Params::default();
        assert_eq!(p.gun_law, GUN_LAW_POWER);
        assert_eq!(
            meta("gun_law").unwrap().values,
            ["power", "gog", "bt1886", "bt1886_annex1"]
        );
        assert_eq!((p.gun_gamma, p.gun_gain, p.gun_offset), (2.4, 1.0, 0.0));
        assert_eq!((p.gun_cutoff_rgb, p.gun_drive_rgb), ([0.0; 3], [1.0; 3]));
        assert_eq!(
            (p.gun_bt1886_white, p.gun_bt1886_black, p.gun_annex1_lift),
            (100.0, 0.1, 0.0)
        );
        assert!(!p.gun_white_balance);
        assert_eq!(
            p.gun_retune_white_xy, p.tube_white_xy,
            "the retune target defaults to the tube white"
        );
        assert_eq!(meta("gun_retune_white_xy").unwrap().file, "params/gun.toml");
        assert_eq!(meta("gun_gamma").unwrap().file, "params/gun.toml");
        assert_eq!(Params::identity_gun().gun_gamma, 1.0);
    }

    #[test]
    fn colour_entries_are_contiguous_vec2() {
        // Four vec2 (size 8, alignment 8) in a row pack without padding, wherever colour.toml lands.
        let base = meta("tube_red_xy").unwrap().offset;
        let rel: Vec<usize> = ["tube_red_xy", "tube_green_xy", "tube_blue_xy", "tube_white_xy"]
            .iter()
            .map(|id| meta(id).unwrap().offset - base)
            .collect();
        assert_eq!(rel, [0, 8, 16, 24]);
    }

    #[test]
    fn members_are_aligned_ordered_and_inside_the_block() {
        let mut end = 0;
        for m in PARAMS_META {
            let align = match m.ty {
                ParamType::Vec2 => 8,
                ParamType::Vec3 | ParamType::Vec4 => 16,
                _ => 4,
            };
            assert_eq!(m.offset % align, 0, "{}", m.id);
            assert!(m.offset >= end, "{} overlaps the previous member", m.id);
            end = m.offset + m.size;
        }
        assert!(end <= PARAMS_STD140_SIZE);
        assert_eq!(PARAMS_STD140_SIZE % 16, 0);
    }

    #[test]
    fn every_entry_has_metadata_text_and_a_valid_default() {
        for m in PARAMS_META {
            assert!(!m.unit.is_empty() && !m.source.is_empty() && !m.description.is_empty() && !m.group.is_empty());
            if let (Some((lo, hi)), ParamValue::F32(v)) = (m.soft_range, m.default) {
                assert!((lo as f32..=hi as f32).contains(&v), "{}", m.id);
            }
        }
    }

    #[test]
    fn std140_bytes_follow_the_metadata_offsets() {
        let mut p = Params::default();
        assert!(p.set("exposure", ParamValue::F32(2.5)));
        let bytes = p.to_std140();
        let o = meta("exposure").unwrap().offset;
        assert_eq!(bytes[o..o + 4], 2.5_f32.to_le_bytes());
        let o = meta("tube_white_xy").unwrap().offset;
        assert_eq!(bytes[o..o + 4], 0.3127_f32.to_le_bytes());
        assert_eq!(bytes[o + 4..o + 8], 0.3290_f32.to_le_bytes());
    }

    #[test]
    fn set_rejects_unknown_ids_and_wrong_types() {
        let mut p = Params::default();
        assert!(!p.set("no_such_parameter", ParamValue::F32(1.0)));
        assert!(!p.set("exposure", ParamValue::I32(1)));
        assert!(!p.set("tube_red_xy", ParamValue::Vec3([0.0; 3])));
        assert_eq!(p, Params::default());
        assert!(p.set("tube_red_xy", ParamValue::Vec2([0.64, 0.33])));
        assert_eq!(p.tube_red_xy, [0.64, 0.33]);
    }

    /// Every float component of `v` (empty for bool, enum and i32).
    fn floats(v: ParamValue) -> Vec<f32> {
        match v {
            ParamValue::F32(x) => vec![x],
            ParamValue::Vec2(a) => a.to_vec(),
            ParamValue::Vec3(a) => a.to_vec(),
            ParamValue::Vec4(a) => a.to_vec(),
            _ => Vec::new(),
        }
    }

    /// `x` in every component of the type of `m` (F32, I32 rounded, vectors).
    fn filled(m: &ParamMeta, x: f64) -> Option<ParamValue> {
        let f = x as f32;
        Some(match m.ty {
            ParamType::F32 => ParamValue::F32(f),
            ParamType::I32 => ParamValue::I32(x.clamp(i32::MIN as f64, i32::MAX as f64) as i32),
            ParamType::Vec2 => ParamValue::Vec2([f; 2]),
            ParamType::Vec3 => ParamValue::Vec3([f; 3]),
            ParamType::Vec4 => ParamValue::Vec4([f; 4]),
            ParamType::Bool | ParamType::Enum => return None,
        })
    }

    #[test]
    fn every_numeric_parameter_has_a_hard_range_holding_its_soft_range_and_default() {
        for m in PARAMS_META {
            match (m.soft_range, m.hard_range) {
                (Some((lo, hi)), Some((hlo, hhi))) => {
                    assert!(hlo <= lo && hi <= hhi, "{}", m.id);
                    assert!(in_hard_range(m, m.default), "{}: default outside the hard range", m.id);
                }
                (None, None) => assert!(matches!(m.ty, ParamType::Bool | ParamType::Enum), "{}", m.id),
                _ => panic!("{}: soft and hard ranges disagree", m.id),
            }
            // Every production entry outside phosphor.toml (owned by the physics work package) documents it.
            if m.soft_range.is_some() && !m.file.starts_with("params/_") && m.file != "params/phosphor.toml" {
                assert!(!m.hard_why.is_empty(), "{}: no hard_why", m.id);
            }
        }
        let aspect = meta("tube_aspect").unwrap();
        assert_eq!(aspect.hard_range, Some((0.25, 4.0)));
        assert!(aspect.hard_why.contains("empty tube domain"));
    }

    /// NaN and infinities are rejected for every float parameter (every component), and nothing changes.
    #[test]
    fn non_finite_values_are_rejected_for_every_float_parameter() {
        for m in PARAMS_META {
            for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
                let Some(v) = filled(m, bad).filter(|_| m.ty != ParamType::I32) else {
                    continue;
                };
                let mut p = Params::default();
                assert_eq!(p.set_checked(m.id, v), Err(Rejected::NonFinite), "{} = {bad}", m.id);
                assert!(!p.set(m.id, v), "{}", m.id);
                assert_eq!(p, Params::default(), "{}", m.id);
            }
            // One bad component is enough.
            let mut p = Params::default();
            let one = match m.default {
                ParamValue::Vec2(mut a) => {
                    a[1] = f32::NAN;
                    ParamValue::Vec2(a)
                }
                ParamValue::Vec3(mut a) => {
                    a[2] = f32::INFINITY;
                    ParamValue::Vec3(a)
                }
                ParamValue::Vec4(mut a) => {
                    a[0] = f32::NEG_INFINITY;
                    ParamValue::Vec4(a)
                }
                _ => continue,
            };
            assert_eq!(p.set_checked(m.id, one), Err(Rejected::NonFinite), "{}", m.id);
            assert_eq!(p, Params::default());
        }
        clear_notices();
    }

    /// 0, negative and huge values: every numeric parameter ends inside its hard range; a value already inside
    /// it is stored as given (unless a cross-parameter rule applies).
    #[test]
    fn zero_negative_and_huge_values_are_clamped_into_the_hard_range() {
        for m in PARAMS_META {
            let (hlo, hhi) = match m.hard_range {
                Some(r) => r,
                None => continue,
            };
            for x in [
                0.0,
                -1.0,
                -1e30,
                1e30,
                f64::from(f32::MAX),
                f64::from(i32::MIN),
                f64::from(i32::MAX),
                hlo,
                hhi,
            ] {
                let v = filled(m, x).unwrap();
                let mut p = Params::default();
                let r = p.set_checked(m.id, v).unwrap_or_else(|e| panic!("{} = {x}: {e}", m.id));
                let stored = p.get(m.id).unwrap();
                assert!(in_hard_range(m, stored), "{} = {x} stored {stored:?}", m.id);
                if in_hard_range(m, v) && constraint_rule(m.id).is_none() {
                    assert_eq!(r, Applied::Exact, "{} = {x}", m.id);
                    assert_eq!(stored, v);
                } else if !in_hard_range(m, v) {
                    assert_eq!(r, Applied::Clamped(stored), "{} = {x}", m.id);
                    // Without a cross-parameter rule the clamp lands on a bound.
                    for c in floats(stored).into_iter().filter(|_| constraint_rule(m.id).is_none()) {
                        assert!(c == hlo as f32 || c == hhi as f32, "{} = {x}: {c} not a bound", m.id);
                    }
                }
            }
        }
        clear_notices();
    }

    #[test]
    fn enum_indices_are_checked_against_the_value_list() {
        for m in PARAMS_META.iter().filter(|m| m.ty == ParamType::Enum) {
            let n = m.values.len() as u32;
            let mut p = Params::default();
            assert_eq!(
                p.set_checked(m.id, ParamValue::Enum(n - 1)),
                Ok(Applied::Exact),
                "{}",
                m.id
            );
            for bad in [n, n + 1, u32::MAX] {
                let before = p;
                assert_eq!(
                    p.set_checked(m.id, ParamValue::Enum(bad)),
                    Err(Rejected::NotAValue),
                    "{}",
                    m.id
                );
                assert_eq!(p, before);
            }
            assert_eq!(p.set_checked(m.id, ParamValue::F32(0.0)), Err(Rejected::WrongType));
        }
        let mut p = Params::default();
        assert_eq!(
            p.set_checked("mask_enabled", ParamValue::Bool(false)),
            Ok(Applied::Exact)
        );
        assert_eq!(p.set_checked("nope", ParamValue::Bool(false)), Err(Rejected::UnknownId));
    }

    /// The owner's failure: 0 typed or loaded into a tube size gives a positive size, never an empty domain.
    #[test]
    fn zero_tube_sizes_are_clamped_to_positive_values() {
        let mut p = Params::default();
        for id in [
            "tube_aspect",
            "tube_visible_height_mm",
            "tube_height_fraction",
            "source_par",
        ] {
            assert!(p.set(id, ParamValue::F32(0.0)), "{id}");
            let ParamValue::F32(v) = p.get(id).unwrap() else {
                unreachable!()
            };
            assert!(v > 0.0, "{id} = {v}");
            assert!(
                notices().iter().any(|n| n.contains(id) && n.contains("valid range")),
                "{id}"
            );
        }
        assert_eq!((p.tube_aspect, p.tube_visible_height_mm), (0.25, 10.0));
        assert_eq!((p.tube_height_fraction, p.source_par), (0.05, 0.1));
        clear_notices();
    }

    /// R-49: halation + glare stay at most 1 whatever the order; the value just set yields; the direct weight
    /// 1 - h - g is never negative, so the three weights partition the light (energy kept).
    #[test]
    fn optics_weights_keep_the_energy_whatever_the_order() {
        for (first, second) in [
            ("optics_halation_weight", "optics_glare_weight"),
            ("optics_glare_weight", "optics_halation_weight"),
        ] {
            let mut p = Params::default();
            assert_eq!(p.set_checked(first, ParamValue::F32(0.8)), Ok(Applied::Exact));
            let r = p.set_checked(second, ParamValue::F32(0.8));
            let (h, g) = (p.optics_halation_weight, p.optics_glare_weight);
            assert_eq!(p.get(first), Some(ParamValue::F32(0.8)), "the earlier value is kept");
            assert!((p.get(second) == Some(ParamValue::F32(1.0 - 0.8))), "{h} {g}");
            assert_eq!(r, Ok(Applied::Clamped(ParamValue::F32(1.0 - 0.8))));
            assert!(1.0 - h - g >= 0.0 && h + g <= 1.0, "{h} + {g}");
            let msg = clamp_message(second, ParamValue::F32(0.8), p.get(second).unwrap());
            assert!(msg.contains("must not sum above 1"), "{msg}");
        }
        // Each one alone may take the whole light; inside the sum nothing is touched.
        let mut p = Params::default();
        assert_eq!(
            p.set_checked("optics_glare_weight", ParamValue::F32(0.0)),
            Ok(Applied::Exact)
        );
        assert_eq!(
            p.set_checked("optics_halation_weight", ParamValue::F32(1.0)),
            Ok(Applied::Exact)
        );
        assert_eq!(
            p.set_checked("optics_glare_weight", ParamValue::F32(0.5)),
            Ok(Applied::Clamped(ParamValue::F32(0.0)))
        );
        // Diffusion is a separate convex blend, not part of the sum.
        assert_eq!(
            p.set_checked("optics_diffusion_weight", ParamValue::F32(1.0)),
            Ok(Applied::Exact)
        );
        assert_eq!(
            p.set_checked("optics_diffusion_weight", ParamValue::F32(1.5)),
            Ok(Applied::Clamped(ParamValue::F32(1.0)))
        );
    }

    /// The glow (bloom) weight joins the energy sum: halation + glare + bloom <= 1 whatever the order, the value
    /// just set yields, and the bloom's own hard range (0.6) applies first.
    #[test]
    fn the_bloom_weight_joins_the_optics_energy_sum() {
        let f = ParamValue::F32;
        let mut p = Params::default();
        assert_eq!(p.set_checked("optics_halation_weight", f(0.3)), Ok(Applied::Exact));
        assert_eq!(p.set_checked("optics_glare_weight", f(0.3)), Ok(Applied::Exact));
        let room = 1.0f32 - 0.3 - 0.3;
        assert_eq!(
            p.set_checked("optics_bloom_weight", f(0.6)),
            Ok(Applied::Clamped(f(room)))
        );
        let msg = clamp_message("optics_bloom_weight", f(0.6), p.get("optics_bloom_weight").unwrap());
        assert!(msg.contains("glow") && msg.contains("must not sum above 1"), "{msg}");
        // Raising the halation now yields to the glare and the bloom.
        assert!(matches!(
            p.set_checked("optics_halation_weight", f(0.9)),
            Ok(Applied::Clamped(_))
        ));
        let sum = p.optics_halation_weight + p.optics_glare_weight + p.optics_bloom_weight;
        assert!(sum <= 1.0 + 1e-6, "{sum}");
        // Alone, the bloom is limited by its hard maximum.
        let mut q = Params {
            optics_halation_weight: 0.0,
            optics_glare_weight: 0.0,
            ..Params::default()
        };
        assert_eq!(
            q.set_checked("optics_bloom_weight", f(0.9)),
            Ok(Applied::Clamped(f(0.6)))
        );
    }

    #[test]
    fn a_forced_display_aspect_is_off_or_at_least_the_minimum() {
        let mut p = Params::default();
        assert_eq!(
            p.set_checked("source_display_aspect", ParamValue::F32(0.0)),
            Ok(Applied::Exact)
        );
        assert_eq!(
            p.set_checked("source_display_aspect", ParamValue::F32(1.5)),
            Ok(Applied::Exact)
        );
        assert_eq!(
            p.set_checked("source_display_aspect", ParamValue::F32(1e-6)),
            Ok(Applied::Clamped(ParamValue::F32(MIN_DISPLAY_ASPECT)))
        );
        assert_eq!(
            p.set_checked("source_display_aspect", ParamValue::F32(-3.0)),
            Ok(Applied::Clamped(ParamValue::F32(0.0)))
        );
    }

    #[test]
    fn set_records_clamps_and_rejections_as_deduplicated_notices() {
        clear_notices();
        let mut p = Params::default();
        assert!(p.set("exposure", ParamValue::F32(1.0)));
        assert!(notices().is_empty(), "an exact value is no notice");
        assert!(p.set("exposure", ParamValue::F32(1e9)));
        assert!(p.set("exposure", ParamValue::F32(1e9)));
        assert!(!p.set("exposure", ParamValue::F32(f32::NAN)));
        let n = notices();
        assert_eq!(n.len(), 2, "{n:?}");
        assert!(
            n[0].contains("'exposure' = 1000000000 is outside its valid range 0.001 to 100"),
            "{}",
            n[0]
        );
        assert!(n[0].contains("set to 100"), "{}", n[0]);
        assert!(n[1].contains("not a finite number"), "{}", n[1]);
        for k in 0..2 * MAX_NOTICES {
            assert!(p.set("ageing_hours", ParamValue::F32(-1.0 - k as f32)));
        }
        assert_eq!(notices().len(), MAX_NOTICES);
        assert!(notices().last().unwrap().contains(&format!("-{}", 2 * MAX_NOTICES)));
        clear_notices();
        assert!(notices().is_empty());
    }

    #[test]
    fn glsl_header_declares_every_member_at_its_metadata_offset() {
        assert!(PARAMS_GLSL.contains(&format!("// std140, {PARAMS_STD140_SIZE} bytes.")));
        for m in PARAMS_META {
            let glsl_type = match m.ty {
                ParamType::F32 => "float",
                ParamType::I32 => "int",
                ParamType::Bool => "bool",
                ParamType::Enum => "uint",
                ParamType::Vec2 => "vec2",
                ParamType::Vec3 => "vec3",
                ParamType::Vec4 => "vec4",
            };
            let line = format!("    layout(offset = {}) {glsl_type} {};", m.offset, m.id);
            assert!(PARAMS_GLSL.contains(&line), "missing `{line}` in params.glsl");
        }
    }

    /// Mixed members of `params/_test.toml`, placed after every production entry. The first test entry is a
    /// vec4, so its offset is 16-aligned and the offsets relative to it are fixed.
    #[cfg(phoxium_test_params)]
    #[test]
    fn test_entries_appear_in_rust_glsl_and_metadata_with_std140_offsets() {
        let p = Params::default();
        assert_eq!(p.test_vec4, [0.5, 0.5, 0.5, 1.0]);
        assert_eq!(p.test_scalar, 0.25);
        assert_eq!(p.test_vec3, [1.0, 2.0, 3.0]);
        assert_eq!(p.test_count, -2);
        assert!(p.test_flag);
        assert_eq!(p.test_mode, TEST_MODE_SECOND);

        // Relative to the vec4: float at 16; vec3 aligned to 32; int fills the vec3's fourth slot (44);
        // bool 48; enum 52; 56 bytes used, block rounded up to 16 (64).
        let test: Vec<&ParamMeta> = PARAMS_META.iter().filter(|m| m.file == "params/_test.toml").collect();
        let base = test[0].offset;
        assert_eq!(base % 16, 0);
        assert!(PARAMS_META
            .iter()
            .all(|m| m.file == "params/_test.toml" || m.offset < base));
        let rel: Vec<(&str, usize)> = test.iter().map(|m| (m.id, m.offset - base)).collect();
        assert_eq!(
            rel,
            [
                ("test_vec4", 0),
                ("test_scalar", 16),
                ("test_vec3", 32),
                ("test_count", 44),
                ("test_flag", 48),
                ("test_mode", 52),
            ]
        );
        assert_eq!(PARAMS_STD140_SIZE, base + 64);
        assert_eq!(meta("test_mode").unwrap().values, ["first", "second", "third"]);
        assert_eq!(meta("test_vec3").unwrap().confidence, Confidence::Derived);
        assert_eq!(meta("test_count").unwrap().soft_range, Some((-5.0, 5.0)));
        assert_eq!(meta("test_flag").unwrap().soft_range, None);
        assert!(PARAMS_GLSL.contains("const uint TEST_MODE_THIRD = 2u;"));

        let bytes = p.to_std140();
        let at = |rel: usize| base + rel..base + rel + 4;
        assert_eq!(bytes[at(12)], 1.0_f32.to_le_bytes());
        assert_eq!(bytes[at(16)], 0.25_f32.to_le_bytes());
        assert_eq!(bytes[at(40)], 3.0_f32.to_le_bytes());
        assert_eq!(bytes[at(44)], (-2_i32).to_le_bytes());
        assert_eq!(bytes[at(48)], 1_u32.to_le_bytes());
        assert_eq!(bytes[at(52)], 1_u32.to_le_bytes());

        let mut q = p;
        assert!(!q.set("test_mode", ParamValue::Enum(3)));
        assert!(q.set("test_mode", ParamValue::Enum(TEST_MODE_THIRD)));
        assert_eq!(q.to_std140()[at(52)], 2_u32.to_le_bytes());
    }
}
