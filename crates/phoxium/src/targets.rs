//! Source colour targets as data files (TODO, "A GBC target"; QUESTIONS.md, "Study: source colour targets").
//!
//! A target is a TOML file of `presets/targets/`, embedded in the binary with `include_str!`; the parameter
//! `source_colour_target` (`params/source.toml`) names it by the id, which is also its value name. Two kinds exist
//! (the formulas are in `crt_model::target`):
//!
//! ```toml
//! schema = 1
//! id = "game_boy_color"                  # the value name of `source_colour_target`
//! name = "Nintendo Game Boy Color"
//! label = "..."                         # one line for the menu
//! kind = "matrix"                       # "matrix" or "curve"
//! notes = "..."                         # what the numbers are, how good, what is not modelled
//! [matrix]                              # kind = "matrix"
//! rows = [[26, 4, 2], [0, 24, 8], [6, 4, 22]]   # one row per output channel R, G, B
//! input_bits = 5                        # bits per channel of the device code, 4 to 8
//! input_range = 31                      # the linearised input at the largest code
//! input_gamma = 1                       # optional, default 1
//! clamp = 960                           # optional, default none
//! divisor = 1020
//! output_gamma = 1                      # optional, default 1
//! output_scale = 1                      # optional, default 1
//! [curve]                               # kind = "curve"
//! table = [0, 1, 3, ...]                # 32 output codes, one per 5-bit input code
//! input_bits = 5
//! output_max = 255
//! [provenance]                          # one entry per key of the kind's table: "<S|D|K|E>: <source>"
//! rows = "E: near.sh, ..."
//! ```
//!
//! [`Target::parse`] rejects an unknown key, a schema other than 1, an unknown kind, a table of the other kind, a value
//! that is not finite or not in range, and a key with no provenance entry of the form `<tag>: <text>`. A target ships
//! only when the source of its values can be named (QUESTIONS.md, Study finding 2).

use std::{collections::BTreeMap, sync::OnceLock};

use crt_model::target::{Curve, Matrix, Transform, CURVE_LEN};
use serde::Deserialize;

/// The schema version of the files.
pub const SCHEMA_VERSION: u32 = 1;

/// The shipped files, in the order of the enum values after `none` (a test checks the ids).
const SHIPPED: &[(&str, &str)] = &[
    (
        "game_boy_color.toml",
        include_str!("../../../presets/targets/game_boy_color.toml"),
    ),
    (
        "game_boy_advance.toml",
        include_str!("../../../presets/targets/game_boy_advance.toml"),
    ),
    (
        "super_nintendo_black_boost.toml",
        include_str!("../../../presets/targets/super_nintendo_black_boost.toml"),
    ),
];

/// A source colour target.
#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    /// The value name of `source_colour_target`.
    pub id: String,
    pub name: String,
    /// One line for the menu.
    pub label: String,
    pub notes: String,
    /// The transform of the device.
    pub transform: Transform,
    /// The provenance entry of every key of the kind's table.
    pub provenance: BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    schema: u32,
    id: String,
    name: String,
    label: String,
    kind: String,
    notes: String,
    matrix: Option<MatrixFile>,
    curve: Option<CurveFile>,
    provenance: BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MatrixFile {
    rows: [[f64; 3]; 3],
    input_bits: u32,
    input_range: f64,
    input_gamma: Option<f64>,
    clamp: Option<f64>,
    divisor: f64,
    output_gamma: Option<f64>,
    output_scale: Option<f64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CurveFile {
    table: Vec<u32>,
    input_bits: u32,
    output_max: f64,
}

fn positive(file: &str, table: &str, key: &str, v: f64) -> Result<(), String> {
    if v.is_finite() && v > 0.0 {
        Ok(())
    } else {
        Err(format!("{file}: {table}.{key} must be finite and positive"))
    }
}

impl MatrixFile {
    /// The keys that are present, for the provenance check.
    fn keys(&self) -> Vec<&'static str> {
        let mut k = vec!["rows", "input_bits", "input_range", "divisor"];
        for (name, present) in [
            ("input_gamma", self.input_gamma.is_some()),
            ("clamp", self.clamp.is_some()),
            ("output_gamma", self.output_gamma.is_some()),
            ("output_scale", self.output_scale.is_some()),
        ] {
            if present {
                k.push(name);
            }
        }
        k
    }

    fn matrix(&self, file: &str) -> Result<Matrix, String> {
        if !self.rows.iter().flatten().all(|w| w.is_finite() && *w >= 0.0) {
            return Err(format!("{file}: matrix.rows must be finite and not negative"));
        }
        if !(4..=8).contains(&self.input_bits) {
            return Err(format!("{file}: matrix.input_bits must be 4 to 8"));
        }
        positive(file, "matrix", "input_range", self.input_range)?;
        positive(file, "matrix", "divisor", self.divisor)?;
        let input_gamma = self.input_gamma.unwrap_or(1.0);
        let output_gamma = self.output_gamma.unwrap_or(1.0);
        let output_scale = self.output_scale.unwrap_or(1.0);
        positive(file, "matrix", "input_gamma", input_gamma)?;
        positive(file, "matrix", "output_gamma", output_gamma)?;
        positive(file, "matrix", "output_scale", output_scale)?;
        if let Some(c) = self.clamp {
            positive(file, "matrix", "clamp", c)?;
        }
        Ok(Matrix {
            rows: self.rows,
            input_bits: self.input_bits,
            input_range: self.input_range,
            input_gamma,
            clamp: self.clamp.unwrap_or(f64::INFINITY),
            divisor: self.divisor,
            output_gamma,
            output_scale,
        })
    }
}

impl CurveFile {
    fn curve(&self, file: &str) -> Result<Curve, String> {
        if self.input_bits != 5 {
            return Err(format!(
                "{file}: curve.input_bits must be 5 (a table of {CURVE_LEN} entries)"
            ));
        }
        if self.table.len() != CURVE_LEN {
            return Err(format!(
                "{file}: curve.table must have {CURVE_LEN} entries, it has {}",
                self.table.len()
            ));
        }
        positive(file, "curve", "output_max", self.output_max)?;
        if self.output_max > 255.0 {
            return Err(format!("{file}: curve.output_max must be at most 255"));
        }
        let mut table = [0u8; CURVE_LEN];
        for (o, v) in table.iter_mut().zip(&self.table) {
            *o = u8::try_from(*v).map_err(|_| format!("{file}: curve.table entries must be 0 to 255"))?;
        }
        Ok(Curve {
            table,
            output_max: self.output_max,
        })
    }
}

impl Target {
    /// Parses and validates the text of the file `file`; the error names the file and the key.
    pub fn parse(file: &str, text: &str) -> Result<Target, String> {
        let f: File = toml::from_str(text).map_err(|e| format!("{file}: {e}"))?;
        if f.schema != SCHEMA_VERSION {
            return Err(format!("{file}: schema {} is not {SCHEMA_VERSION}", f.schema));
        }
        if f.id.is_empty()
            || !f
                .id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        {
            return Err(format!(
                "{file}: id \"{}\" must be lower case letters, digits and '_'",
                f.id
            ));
        }
        let (transform, keys) = match (f.kind.as_str(), &f.matrix, &f.curve) {
            ("matrix", Some(m), None) => (Transform::Matrix(m.matrix(file)?), m.keys()),
            ("curve", None, Some(c)) => (
                Transform::Curve(c.curve(file)?),
                vec!["table", "input_bits", "output_max"],
            ),
            ("matrix" | "curve", _, _) => {
                return Err(format!("{file}: kind \"{}\" needs its own table and no other", f.kind));
            }
            (other, _, _) => return Err(format!("{file}: kind \"{other}\" is not \"matrix\" or \"curve\"")),
        };
        for key in keys {
            let ok = f.provenance.get(key).is_some_and(|p| {
                matches!(p.split_once(": "), Some((tag, text)) if ["S", "D", "K", "E"].contains(&tag) && !text.trim().is_empty())
            });
            if !ok {
                return Err(format!(
                    "{file}: provenance.{key} is missing or is not \"<S|D|K|E>: <source>\""
                ));
            }
        }
        Ok(Target {
            id: f.id,
            name: f.name,
            label: f.label,
            notes: f.notes,
            transform,
            provenance: f.provenance,
        })
    }
}

/// The shipped targets, parsed once. A malformed shipped file is a bug (a test parses them all); it is left out.
pub fn shipped() -> &'static [Target] {
    static ALL: OnceLock<Vec<Target>> = OnceLock::new();
    ALL.get_or_init(|| {
        SHIPPED
            .iter()
            .filter_map(|(file, text)| Target::parse(file, text).ok())
            .collect()
    })
}

/// The shipped target called `id` (the value name of `source_colour_target`), `None` for `none` and unknown names.
pub fn by_id(id: &str) -> Option<&'static Target> {
    shipped().iter().find(|t| t.id == id)
}

/// The target of the value `value` of `source_colour_target` (`SOURCE_COLOUR_TARGET_*`), `None` for `none`.
pub fn for_value(value: u32) -> Option<&'static Target> {
    let names = crate::params::meta("source_colour_target")?.values;
    by_id(names.get(value as usize)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::{
        meta, Params, SOURCE_COLOUR_TARGET_GAME_BOY_ADVANCE, SOURCE_COLOUR_TARGET_GAME_BOY_COLOR,
        SOURCE_COLOUR_TARGET_NONE, SOURCE_COLOUR_TARGET_SUPER_NINTENDO_BLACK_BOOST,
    };

    /// Every enum value after `none` has a file of that id, in order, and `none` has none: no value without data, no
    /// file that nothing can select.
    #[test]
    fn every_value_has_a_file_and_every_file_a_value() {
        let values = meta("source_colour_target").unwrap().values;
        assert_eq!(values[0], "none");
        assert_eq!(values.len(), SHIPPED.len() + 1);
        for (i, (file, text)) in SHIPPED.iter().enumerate() {
            let t = Target::parse(file, text).unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(t.id, values[i + 1], "{file}");
            assert!(!t.notes.trim().is_empty() && !t.label.trim().is_empty());
            assert!(t
                .provenance
                .values()
                .all(|p| ["S: ", "D: ", "K: ", "E: "].iter().any(|g| p.starts_with(g))));
        }
        assert_eq!(shipped().len(), SHIPPED.len());
        assert!(for_value(SOURCE_COLOUR_TARGET_NONE).is_none());
        assert_eq!(Params::default().source_colour_target, SOURCE_COLOUR_TARGET_NONE);
    }

    /// The Game Boy Color numbers are those of the file (and of the oracle's unit tests).
    #[test]
    fn the_gbc_file_holds_the_published_numbers() {
        let Transform::Matrix(m) = for_value(SOURCE_COLOUR_TARGET_GAME_BOY_COLOR).expect("gbc").transform else {
            panic!("gbc is a matrix");
        };
        assert_eq!(m.rows, [[26.0, 4.0, 2.0], [0.0, 24.0, 8.0], [6.0, 4.0, 22.0]]);
        assert_eq!((m.input_bits, m.input_range, m.input_gamma), (5, 31.0, 1.0));
        assert_eq!((m.clamp, m.divisor), (960.0, 1020.0));
        assert_eq!((m.output_gamma, m.output_scale), (1.0, 1.0));
    }

    /// The Game Boy Advance numbers: the rows of the article are written on (b, g, r), the file on (R, G, B).
    #[test]
    fn the_gba_file_holds_the_published_numbers() {
        let Transform::Matrix(m) = for_value(SOURCE_COLOUR_TARGET_GAME_BOY_ADVANCE).expect("gba").transform else {
            panic!("gba is a matrix");
        };
        assert_eq!(m.rows, [[255.0, 50.0, 0.0], [10.0, 230.0, 30.0], [50.0, 10.0, 220.0]]);
        assert_eq!((m.input_bits, m.input_range, m.input_gamma), (5, 1.0, 4.0));
        assert!(m.clamp.is_infinite());
        assert_eq!((m.divisor, m.output_gamma), (255.0, 2.2));
        assert_eq!(m.output_scale, 255.0 / 280.0);
    }

    /// The SNES table: 32 entries, rising, from 0 to 255, as printed in the article.
    #[test]
    fn the_snes_file_holds_the_published_table() {
        let Transform::Curve(c) = for_value(SOURCE_COLOUR_TARGET_SUPER_NINTENDO_BLACK_BOOST)
            .expect("snes")
            .transform
        else {
            panic!("snes is a curve");
        };
        assert_eq!(
            (c.table[0], c.table[1], c.table[16], c.table[30], c.table[31]),
            (0, 1, 0x88, 0xf8, 0xff)
        );
        assert!(c.table.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(c.output_max, 255.0);
        // The audit's exact form: i (i + 1) / 2 below 16, min(255, 8 (i + 1)) from 16.
        for (i, v) in c.table.iter().enumerate() {
            let want = if i < 16 {
                i * (i + 1) / 2
            } else {
                (8 * (i + 1)).min(255)
            };
            assert_eq!(usize::from(*v), want, "entry {i}");
        }
    }

    /// The validation names the file and the key: an unknown key, a missing provenance entry, a bad schema, a bad kind,
    /// a negative weight, a zero divisor, a wrong table.
    #[test]
    fn a_bad_file_is_refused_with_its_key() {
        let ok = SHIPPED[0].1.to_string();
        let snes = SHIPPED[2].1.to_string();
        assert!(Target::parse("t.toml", &ok).is_ok());
        assert!(Target::parse("t.toml", &snes).is_ok());
        let cases = [
            (ok.replace("schema = 1", "schema = 2"), "schema"),
            (ok.replace("kind = \"matrix\"", "kind = \"lut3d\""), "kind"),
            (ok.replace("kind = \"matrix\"", "kind = \"curve\""), "kind"),
            (ok.replace("clamp = 960", "clamp = 960\nextra = 1"), "extra"),
            (ok.replace("divisor = 1020", "divisor = 0"), "divisor"),
            (ok.replace("[[26, 4, 2],", "[[-26, 4, 2],"), "rows"),
            (ok.replace("clamp = \"E:", "clamp = \"X:"), "provenance.clamp"),
            (
                ok.replace("input_bits = \"S: the", "input_bits_ = \"S: the"),
                "provenance.input_bits",
            ),
            (ok.replace("input_bits = 5", "input_bits = 3"), "input_bits"),
            (snes.replace("input_bits = 5", "input_bits = 4"), "input_bits"),
            (snes.replace("0x00", "0xzz"), "t.toml"),
            (snes.replace("output_max = 255", "output_max = 300"), "output_max"),
        ];
        for (text, key) in cases {
            let err = Target::parse("t.toml", &text).expect_err(key);
            assert!(err.contains("t.toml") && err.contains(key), "{key}: {err}");
        }
    }
}
