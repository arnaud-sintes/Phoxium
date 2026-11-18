//! CRT parameters, generated from the declarative table in `params/*.toml` (TEC-3, DESIGN section 8).
//!
//! `build.rs` runs `crates/paramgen`, which writes `params.rs` (included here) and `params.glsl` (the
//! matching std140 uniform block, see [`PARAMS_GLSL`]) to `OUT_DIR`. Generated items:
//! - [`Params`]: one field per parameter, `Default` gives the table defaults;
//! - [`PARAMS_META`]: id, stage, type, unit, default, soft range, confidence tag, source, description,
//!   UI group, enum values, defining file and std140 offset of every parameter, in layout order;
//! - [`Params::write_std140`] and [`Params::to_std140`]: the uniform buffer bytes ([`PARAMS_STD140_SIZE`]);
//! - [`Params::get`] and [`Params::set`]: access by id for the UI and the presets;
//! - one `u32` constant per enum value, `<ID>_<VALUE>`, also defined in GLSL.
//!
//! Test-only entries (`params/_test.toml`) exist only in non-release builds, under `cfg(ucrt_test_params)`.

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
    /// Soft range (UI range; outside values are allowed with a warning), per component for vectors. `None`
    /// for bool and enum.
    pub soft_range: Option<(f64, f64)>,
    pub confidence: Confidence,
    pub source: &'static str,
    pub description: &'static str,
    /// UI group.
    pub group: &'static str,
    /// Enum choices, empty for other types.
    pub values: &'static [&'static str],
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

/// The generated GLSL header: enum constants and the `Params` uniform block, instance `params`, at set and
/// binding 0 unless `UCRT_PARAMS_SET` and `UCRT_PARAMS_BINDING` are defined before the include. Shaders
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
    #[cfg(ucrt_test_params)]
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
