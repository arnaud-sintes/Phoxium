//! Source colour targets as data files (TODO, "A GBC target"; QUESTIONS.md, "Study: source colour targets").
//!
//! A target is a TOML file of `presets/targets/`, embedded in the binary with `include_str!`; the parameter
//! `source_colour_target` (`params/source.toml`) names it by the id, which is also its value name. Two kinds exist
//! (the formulas are in `crt_model::target`; a third kind, `palette`, below):
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

use crt_model::target::{decode_yuv, Curve, Dac, Matrix, Palette, Signal, Transform, CURVE_LEN, DAC_LEN, SIGNAL_MAX};
use serde::Deserialize;

/// The schema version of the files.
pub const SCHEMA_VERSION: u32 = 1;

/// Palettes a target may hold: the range of `source_colour_variant` (`params/source.toml`).
pub const MAX_PALETTES: usize = 32;

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
    (
        "super_game_boy.toml",
        include_str!("../../../presets/targets/super_game_boy.toml"),
    ),
    (
        "sega_mega_drive.toml",
        include_str!("../../../presets/targets/sega_mega_drive.toml"),
    ),
    (
        "commodore_64.toml",
        include_str!("../../../presets/targets/commodore_64.toml"),
    ),
    (
        "nintendo_entertainment_system.toml",
        include_str!("../../../presets/targets/nintendo_entertainment_system.toml"),
    ),
    (
        "ibm_cga_rgbi.toml",
        include_str!("../../../presets/targets/ibm_cga_rgbi.toml"),
    ),
    (
        "game_boy_color_colorimetric.toml",
        include_str!("../../../presets/targets/game_boy_color_colorimetric.toml"),
    ),
    (
        "game_boy_advance_sp_ags_101.toml",
        include_str!("../../../presets/targets/game_boy_advance_sp_ags_101.toml"),
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
    /// The transform of the device (for a palette target: the first palette).
    pub transform: Transform,
    /// The palettes of a `palette` target or the emulators of a `dac` target, in the order of `source_colour_variant`
    /// (1 is the first; empty for the other kinds).
    pub variants: Vec<Variant>,
    /// The provenance entry of every key of the kind's table.
    pub provenance: BTreeMap<String, String>,
}

/// One variant of a target: a palette of a `palette` target, an emulator input of a `dac` target.
#[derive(Clone, Debug, PartialEq)]
pub struct Variant {
    /// Lower case id (`1_a`).
    pub id: String,
    /// The name the device shows (`1-A`).
    pub name: String,
    pub transform: Transform,
}

impl Target {
    /// The transform for the variant number `variant` (1 based, `source_colour_variant`); a target without variants,
    /// and a number below 1 or past the last variant, give the target's own (first) transform.
    pub fn select(&self, variant: i32) -> Transform {
        self.transform_for(usize::try_from(variant.max(1) - 1).unwrap_or(0))
    }

    /// The transform for the variant `variant` (0 based); targets without variants, and an index past the last
    /// variant, give the target's own (first) transform.
    pub fn transform_for(&self, variant: usize) -> Transform {
        self.variants.get(variant).map_or(self.transform, |v| v.transform)
    }
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
    palette: Option<PaletteFile>,
    dac: Option<DacFile>,
    signal: Option<SignalFile>,
    remap: Option<RemapFile>,
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
    /// Variants of the matrix: each may replace the weights, the input gamma, the divisor and the output scale.
    variants: Option<Vec<MatrixVariant>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MatrixVariant {
    id: String,
    name: String,
    rows: Option<[[f64; 3]; 3]>,
    input_gamma: Option<f64>,
    divisor: Option<f64>,
    output_scale: Option<f64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CurveFile {
    table: Vec<u32>,
    input_bits: u32,
    output_max: f64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DacFile {
    /// The output code of each of the 8 codes.
    output: [u32; DAC_LEN],
    output_max: f64,
    inputs: Vec<DacInput>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DacInput {
    id: String,
    name: String,
    /// The 8-bit level the emulator shows for each code.
    levels: [u32; DAC_LEN],
}

impl DacFile {
    /// The emulator inputs as variants, the first one the default.
    fn variants(&self, file: &str) -> Result<Vec<Variant>, String> {
        positive(file, "dac", "output_max", self.output_max)?;
        if self.output_max > 255.0 {
            return Err(format!("{file}: dac.output_max must be at most 255"));
        }
        let mut output = [0u8; DAC_LEN];
        for (o, v) in output.iter_mut().zip(&self.output) {
            *o = u8::try_from(*v).map_err(|_| format!("{file}: dac.output entries must be 0 to 255"))?;
        }
        if output.windows(2).any(|w| w[0] > w[1]) {
            return Err(format!("{file}: dac.output must not decrease"));
        }
        if self.inputs.is_empty() {
            return Err(format!("{file}: dac.inputs must hold at least one emulator"));
        }
        let mut out: Vec<Variant> = Vec::new();
        for s in &self.inputs {
            if s.id.is_empty()
                || !s
                    .id
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
            {
                return Err(format!(
                    "{file}: dac.inputs id \"{}\"  must be lower case letters, digits and '_'",
                    s.id
                ));
            }
            if out.iter().any(|v| v.id == s.id) {
                return Err(format!("{file}: dac.inputs id \"{}\" is used twice", s.id));
            }
            let mut input = [0u8; DAC_LEN];
            for (o, v) in input.iter_mut().zip(&s.levels) {
                *o = u8::try_from(*v).map_err(|_| format!("{file}: dac.inputs {} levels must be 0 to 255", s.id))?;
            }
            if input.windows(2).any(|w| w[0] >= w[1]) {
                return Err(format!("{file}: dac.inputs {} levels must strictly increase", s.id));
            }
            out.push(Variant {
                id: s.id.clone(),
                name: s.name.clone(),
                transform: Transform::Dac(Dac {
                    input,
                    output,
                    output_max: self.output_max,
                }),
            });
        }
        Ok(out)
    }
}

/// A palette remap: the colours of a fixed palette (the reference, as an emulator shows them) replaced by the colours of a
/// monitor (the output); built on the `signal` transform, whose recognition takes the nearest reference colour.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemapFile {
    /// The largest distance (Euclidean in drive voltage) at which a pixel is taken for a reference colour.
    threshold: f64,
    /// The colours emulators show: an RGB value (0xRRGGBB) and the index of the colour it stands for (several references
    /// may stand for one colour: two emulators' browns).
    references: Vec<RemapReference>,
    variants: Vec<RemapVariant>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemapReference {
    rgb: u32,
    colour: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemapVariant {
    id: String,
    name: String,
    /// The colour each index shows on this monitor, 0xRRGGBB, as the drive voltage over 255 (no gamma).
    output: Vec<u32>,
}

impl RemapFile {
    fn variants(&self, file: &str) -> Result<Vec<Variant>, String> {
        positive(file, "remap", "threshold", self.threshold)?;
        let n = self.references.len();
        if n == 0 || n > SIGNAL_MAX {
            return Err(format!("{file}: remap.references must hold 1 to {SIGNAL_MAX} colours"));
        }
        let colours = self.references.iter().map(|r| r.colour).max().unwrap_or(0) + 1;
        if self.references.iter().any(|r| r.rgb > 0xFF_FFFF) {
            return Err(format!("{file}: remap.references rgb must be at most 0xFFFFFF"));
        }
        if self.variants.is_empty() || self.variants.len() > MAX_PALETTES {
            return Err(format!("{file}: remap.variants must hold 1 to {MAX_PALETTES} monitors"));
        }
        let rgb = |c: u32| [(c >> 16) & 255, (c >> 8) & 255, c & 255].map(|v| f64::from(v) / 255.0);
        let mut references = [[0.0; 3]; SIGNAL_MAX];
        for (i, r) in self.references.iter().enumerate() {
            references[i] = rgb(r.rgb);
        }
        let mut out: Vec<Variant> = Vec::new();
        for v in &self.variants {
            if v.id.is_empty()
                || !v
                    .id
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
            {
                return Err(format!(
                    "{file}: remap.variants id \"{}\" must be lower case letters, digits and '_'",
                    v.id
                ));
            }
            if out.iter().any(|x| x.id == v.id) {
                return Err(format!("{file}: remap.variants id \"{}\" is used twice", v.id));
            }
            if v.output.len() != colours || v.output.iter().any(|c| *c > 0xFF_FFFF) {
                return Err(format!(
                    "{file}: remap.variants {} output must hold {colours} colours of at most 0xFFFFFF",
                    v.id
                ));
            }
            let mut table = [[0.0; 3]; SIGNAL_MAX];
            for (i, r) in self.references.iter().enumerate() {
                table[i] = rgb(v.output[r.colour]);
            }
            out.push(Variant {
                id: v.id.clone(),
                name: v.name.clone(),
                transform: Transform::Signal(Signal {
                    colours: table,
                    count: n,
                    references,
                    threshold: self.threshold,
                }),
            });
        }
        Ok(out)
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SignalFile {
    /// Chroma amplitude of a colour that has no `swing_mv`, in the units of the white-black span of the luma (the
    /// composite model's colour-difference scale).
    amplitude: Option<f64>,
    /// The share of a square wave's swing that the decoder's fundamental keeps (0.6440 for the 12-sample window of the
    /// NES, 2 / pi in the continuous limit): the chroma of a colour with a `swing_mv` is this times the swing.
    chroma_factor: Option<f64>,
    threshold: f64,
    /// The colours whose luma is the black and the white level, for a revision that does not give `black_mv` and
    /// `white_mv`.
    black_index: Option<usize>,
    white_index: Option<usize>,
    /// The model of the reference palette the classification compares with.
    reference: SignalReference,
    colours: Vec<SignalColour>,
    revisions: Vec<SignalRevision>,
}

/// The reference palette of the classification: the colours of the file computed with these settings.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SignalReference {
    /// The level of each colour (replaces the colours' `level_mv`), in the units of `black_mv` and `white_mv`.
    luma_mv: Option<Vec<f64>>,
    black_mv: Option<f64>,
    white_mv: Option<f64>,
    phase_row_deg: Option<f64>,
    /// The chroma amplitude of colours without a swing (replaces the file's).
    amplitude: Option<f64>,
    /// A factor on the chroma only (default 1).
    saturation: Option<f64>,
    /// The contrast applied to luma and chroma.
    gain: f64,
    /// The exponent a PC display applies, to compare with an emulator's colours.
    gamma: f64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SignalColour {
    name: String,
    /// The chroma phase in degrees on the U axis (U = cos, V = sin); none for a grey.
    angle: Option<f64>,
    /// The luma row of the NES palette (0 to 3), which turns the phase by `phase_row_deg` degrees a row.
    row: Option<u32>,
    /// The level of the colour in mV (the mean of a square wave, the constant level of a grey), when the revision gives no
    /// `luma_mv`.
    level_mv: Option<f64>,
    /// The peak-to-peak swing in mV of the square wave that makes the chroma.
    swing_mv: Option<f64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SignalRevision {
    id: String,
    name: String,
    /// The luma level of each colour, in the order of `colours`, in mV (replaces the colours' `level_mv`).
    luma_mv: Option<Vec<f64>>,
    black_mv: Option<f64>,
    white_mv: Option<f64>,
    /// The turn of the chroma phase for each luma row, in degrees (the differential phase distortion; none = 0).
    phase_row_deg: Option<f64>,
}

/// The settings that turn the colours of a signal file into drive voltages (a revision, or the reference model).
struct SignalSettings<'a> {
    luma_mv: Option<&'a [f64]>,
    black_mv: Option<f64>,
    white_mv: Option<f64>,
    phase_row_deg: f64,
    amplitude: Option<f64>,
    saturation: f64,
    gain: f64,
    gamma: f64,
}

impl SignalFile {
    /// The keys that are present, for the provenance check.
    fn keys(&self) -> Vec<&'static str> {
        let mut k = vec!["colours", "revisions", "reference", "threshold"];
        for (name, present) in [
            ("amplitude", self.amplitude.is_some()),
            ("chroma_factor", self.chroma_factor.is_some()),
            ("black_index", self.black_index.is_some()),
            ("white_index", self.white_index.is_some()),
        ] {
            if present {
                k.push(name);
            }
        }
        k
    }

    /// The drive voltages of every colour under `s` (see [`SignalSettings`]).
    fn colours_for(&self, file: &str, what: &str, s: &SignalSettings) -> Result<[[f64; 3]; SIGNAL_MAX], String> {
        let n = self.colours.len();
        let level = |i: usize| -> Result<f64, String> {
            s.luma_mv
                .map(|l| l[i])
                .or(self.colours[i].level_mv)
                .filter(|v| v.is_finite())
                .ok_or_else(|| format!("{file}: {what} has no level for colour {i}"))
        };
        let black = match (s.black_mv, self.black_index) {
            (Some(b), _) => b,
            (None, Some(i)) if i < n => level(i)?,
            _ => return Err(format!("{file}: {what} has no black level (black_mv or black_index)")),
        };
        let white = match (s.white_mv, self.white_index) {
            (Some(w), _) => w,
            (None, Some(i)) if i < n => level(i)?,
            _ => return Err(format!("{file}: {what} has no white level (white_mv or white_index)")),
        };
        if !(black.is_finite() && white.is_finite() && white > black) {
            return Err(format!("{file}: {what}: white must be above black"));
        }
        let span = white - black;
        let mut out = [[0.0; 3]; SIGNAL_MAX];
        for (i, c) in self.colours.iter().enumerate() {
            let y = (level(i)? - black) / span;
            let amplitude = match (c.swing_mv, self.chroma_factor, s.amplitude.or(self.amplitude)) {
                (Some(sw), Some(k), _) => k * sw / span,
                (_, _, Some(a)) => a,
                _ => 0.0,
            };
            let (u, v) = c.angle.map_or((0.0, 0.0), |a| {
                let a = (a + s.phase_row_deg * f64::from(c.row.unwrap_or(0))).to_radians();
                (amplitude * a.cos(), amplitude * a.sin())
            });
            out[i] =
                decode_yuv(y * s.gain, u * s.gain * s.saturation, v * s.gain * s.saturation).map(|c| c.powf(s.gamma));
        }
        Ok(out)
    }

    /// The revisions as variants: each colour's drive voltages from its level, chroma angle and amplitude, and the
    /// reference palette of the classification.
    fn variants(&self, file: &str) -> Result<Vec<Variant>, String> {
        let n = self.colours.len();
        if n == 0 || n > SIGNAL_MAX {
            return Err(format!("{file}: signal.colours must hold 1 to {SIGNAL_MAX} colours"));
        }
        if let Some(a) = self.amplitude {
            positive(file, "signal", "amplitude", a)?;
        }
        if let Some(k) = self.chroma_factor {
            positive(file, "signal", "chroma_factor", k)?;
        }
        positive(file, "signal", "threshold", self.threshold)?;
        positive(file, "signal.reference", "gain", self.reference.gain)?;
        positive(file, "signal.reference", "gamma", self.reference.gamma)?;
        let r = &self.reference;
        for (key, v) in [("amplitude", r.amplitude), ("saturation", r.saturation)] {
            if let Some(v) = v {
                positive(file, "signal.reference", key, v)?;
            }
        }
        for (what, l) in [("signal.reference.luma_mv", &r.luma_mv)] {
            if l.as_ref()
                .is_some_and(|l| l.len() != n || l.iter().any(|v| !v.is_finite() || *v < 0.0))
            {
                return Err(format!("{file}: {what} must hold {n} levels, not negative"));
            }
        }
        if self.colours.iter().any(|c| c.name.trim().is_empty()) {
            return Err(format!("{file}: signal.colours name must not be empty"));
        }
        if self.colours.iter().any(|c| {
            [c.angle, c.level_mv, c.swing_mv]
                .iter()
                .flatten()
                .any(|v| !v.is_finite())
        }) {
            return Err(format!(
                "{file}: signal.colours angle, level_mv and swing_mv must be finite"
            ));
        }
        if self.colours.iter().any(|c| c.swing_mv.is_some()) && self.chroma_factor.is_none() {
            return Err(format!(
                "{file}: signal.chroma_factor is needed by the colours with a swing_mv"
            ));
        }
        if self.revisions.is_empty() || self.revisions.len() > MAX_PALETTES {
            return Err(format!(
                "{file}: signal.revisions must hold 1 to {MAX_PALETTES} revisions"
            ));
        }
        let references = self.colours_for(
            file,
            "signal.reference",
            &SignalSettings {
                luma_mv: r.luma_mv.as_deref(),
                black_mv: r.black_mv,
                white_mv: r.white_mv,
                phase_row_deg: r.phase_row_deg.unwrap_or(0.0),
                amplitude: r.amplitude,
                saturation: r.saturation.unwrap_or(1.0),
                gain: r.gain,
                gamma: r.gamma,
            },
        )?;
        let mut out: Vec<Variant> = Vec::new();
        for rev in &self.revisions {
            if rev.id.is_empty()
                || !rev
                    .id
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
            {
                return Err(format!(
                    "{file}: signal.revisions id \"{}\" must be lower case letters, digits and '_'",
                    rev.id
                ));
            }
            if out.iter().any(|v| v.id == rev.id) {
                return Err(format!("{file}: signal.revisions id \"{}\" is used twice", rev.id));
            }
            if rev
                .luma_mv
                .as_ref()
                .is_some_and(|l| l.len() != n || l.iter().any(|v| !v.is_finite() || *v <= 0.0))
            {
                return Err(format!(
                    "{file}: signal.revisions {} luma_mv must hold {n} positive levels",
                    rev.id
                ));
            }
            let colours = self.colours_for(
                file,
                &format!("signal.revisions {}", rev.id),
                &SignalSettings {
                    luma_mv: rev.luma_mv.as_deref(),
                    black_mv: rev.black_mv,
                    white_mv: rev.white_mv,
                    phase_row_deg: rev.phase_row_deg.unwrap_or(0.0),
                    amplitude: None,
                    saturation: 1.0,
                    gain: 1.0,
                    gamma: 1.0,
                },
            )?;
            out.push(Variant {
                id: rev.id.clone(),
                name: rev.name.clone(),
                transform: Transform::Signal(Signal {
                    colours,
                    count: n,
                    references,
                    threshold: self.threshold,
                }),
            });
        }
        Ok(out)
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PaletteFile {
    /// Bits per colour channel of the words (5: RGB555, red in bits 0 to 4, green 5 to 9, blue 10 to 14).
    colour_bits: u32,
    sets: Vec<PaletteSet>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PaletteSet {
    id: String,
    name: String,
    /// Four colour words, lightest first.
    words: [u32; 4],
}

impl PaletteFile {
    /// The palettes as variants, each with its four colours decoded from the words.
    fn variants(&self, file: &str) -> Result<Vec<Variant>, String> {
        if self.colour_bits != 5 {
            return Err(format!("{file}: palette.colour_bits must be 5 (RGB555 words)"));
        }
        if self.sets.is_empty() || self.sets.len() > MAX_PALETTES {
            return Err(format!("{file}: palette.sets must hold 1 to {MAX_PALETTES} palettes"));
        }
        let mut out: Vec<Variant> = Vec::new();
        for s in &self.sets {
            if s.id.is_empty()
                || !s
                    .id
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
            {
                return Err(format!(
                    "{file}: palette.sets id \"{}\" must be lower case letters, digits and '_'",
                    s.id
                ));
            }
            if out.iter().any(|v| v.id == s.id) {
                return Err(format!("{file}: palette.sets id \"{}\" is used twice", s.id));
            }
            if s.words.iter().any(|w| *w > 0x7FFF) {
                return Err(format!("{file}: palette {} has a word above 0x7FFF (15 bits)", s.id));
            }
            let colour = |w: u32| [(w & 31) as u8, ((w >> 5) & 31) as u8, ((w >> 10) & 31) as u8];
            out.push(Variant {
                id: s.id.clone(),
                name: s.name.clone(),
                transform: Transform::Palette(Palette {
                    colours: s.words.map(colour),
                    output_max: 31.0,
                }),
            });
        }
        Ok(out)
    }
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
            ("variants", self.variants.is_some()),
        ] {
            if present {
                k.push(name);
            }
        }
        k
    }

    /// The variants of the matrix: the base matrix with each variant's replacements, the first one the target's own.
    fn matrix_variants(&self, file: &str, base: &Matrix) -> Result<Vec<Variant>, String> {
        let Some(list) = &self.variants else {
            return Ok(Vec::new());
        };
        if list.is_empty() || list.len() > MAX_PALETTES {
            return Err(format!(
                "{file}: matrix.variants must hold 1 to {MAX_PALETTES} variants"
            ));
        }
        let mut out: Vec<Variant> = Vec::new();
        for v in list {
            if v.id.is_empty()
                || !v
                    .id
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
            {
                return Err(format!(
                    "{file}: matrix.variants id \"{}\" must be lower case letters, digits and '_'",
                    v.id
                ));
            }
            if out.iter().any(|x| x.id == v.id) {
                return Err(format!("{file}: matrix.variants id \"{}\" is used twice", v.id));
            }
            let mut m = *base;
            if let Some(rows) = v.rows {
                if !rows.iter().flatten().all(|w| w.is_finite()) {
                    return Err(format!("{file}: matrix.variants {} rows must be finite", v.id));
                }
                m.rows = rows;
            }
            for (key, value, slot) in [
                ("input_gamma", v.input_gamma, &mut m.input_gamma),
                ("divisor", v.divisor, &mut m.divisor),
                ("output_scale", v.output_scale, &mut m.output_scale),
            ] {
                if let Some(x) = value {
                    positive(file, "matrix.variants", key, x)?;
                    *slot = x;
                }
            }
            out.push(Variant {
                id: v.id.clone(),
                name: v.name.clone(),
                transform: Transform::Matrix(m),
            });
        }
        Ok(out)
    }

    fn matrix(&self, file: &str) -> Result<Matrix, String> {
        if !self.rows.iter().flatten().all(|w| w.is_finite()) {
            return Err(format!("{file}: matrix.rows must be finite"));
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
        let mut variants = Vec::new();
        // Which table the kind names, and that it is the only one the file has.
        let which = match f.kind.as_str() {
            "matrix" => 0,
            "curve" => 1,
            "palette" => 2,
            "dac" => 3,
            "signal" => 4,
            "remap" => 5,
            other => {
                return Err(format!(
                "{file}: kind \"{other}\" is not \"matrix\", \"curve\", \"palette\", \"dac\", \"signal\" or \"remap\""
            ))
            }
        };
        let tables = [
            f.matrix.is_some(),
            f.curve.is_some(),
            f.palette.is_some(),
            f.dac.is_some(),
            f.signal.is_some(),
            f.remap.is_some(),
        ];
        if tables.iter().enumerate().any(|(i, has)| *has != (i == which)) {
            return Err(format!("{file}: kind \"{}\" needs its own table and no other", f.kind));
        }
        let (transform, keys) = match (&f.matrix, &f.curve, &f.palette, &f.dac, &f.signal, &f.remap) {
            (Some(m), ..) => {
                let base = m.matrix(file)?;
                variants = m.matrix_variants(file, &base)?;
                let first = variants.first().map_or(Transform::Matrix(base), |v| v.transform);
                (first, m.keys())
            }
            (_, Some(c), ..) => (
                Transform::Curve(c.curve(file)?),
                vec!["table", "input_bits", "output_max"],
            ),
            (_, _, Some(p), ..) => {
                variants = p.variants(file)?;
                (variants[0].transform, vec!["sets", "colour_bits"])
            }
            (_, _, _, Some(d), ..) => {
                variants = d.variants(file)?;
                (variants[0].transform, vec!["output", "output_max", "inputs"])
            }
            (_, _, _, _, Some(s), _) => {
                variants = s.variants(file)?;
                (variants[0].transform, s.keys())
            }
            (_, _, _, _, _, Some(r)) => {
                variants = r.variants(file)?;
                (variants[0].transform, vec!["references", "variants", "threshold"])
            }
            _ => unreachable!("the kind has its table"),
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
            variants,
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

/// The colours of the chosen signal target for the `target_table` of the frame uniform buffer
/// (`gpu::FRAME_TABLE_VEC4` entries): the hardware colours (drive voltages) at entries 0 to count, the reference colours
/// the classification compares with at entries 64 to 64 + count; `None` when the target is not a signal one.
pub fn frame_table(p: &crate::params::Params) -> Option<Vec<[f32; 4]>> {
    let t = for_value(p.source_colour_target)?;
    match t.select(p.source_colour_variant) {
        Transform::Signal(s) => {
            let mut table = vec![[0.0f32; 4]; SIGNAL_MAX + s.count];
            for i in 0..s.count {
                table[i] = [
                    s.colours[i][0] as f32,
                    s.colours[i][1] as f32,
                    s.colours[i][2] as f32,
                    0.0,
                ];
                table[SIGNAL_MAX + i] = [
                    s.references[i][0] as f32,
                    s.references[i][1] as f32,
                    s.references[i][2] as f32,
                    0.0,
                ];
            }
            Some(table)
        }
        _ => None,
    }
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
        SOURCE_COLOUR_TARGET_NONE, SOURCE_COLOUR_TARGET_SEGA_MEGA_DRIVE, SOURCE_COLOUR_TARGET_SUPER_GAME_BOY,
        SOURCE_COLOUR_TARGET_SUPER_NINTENDO_BLACK_BOOST,
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
            (ok.replace("[[26, 4, 2],", "[[inf, 4, 2],"), "rows"),
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

    /// The Super Game Boy file: 32 palettes in the menu order 1-A to 4-H, the first decoded by hand (0x67BF is
    /// 0b0_11001_11101_11111: red 31, green 29, blue 25), the last one 4-H, and the first palette is the target's own.
    #[test]
    fn the_sgb_file_holds_32_palettes_in_menu_order() {
        let t = for_value(SOURCE_COLOUR_TARGET_SUPER_GAME_BOY).expect("sgb");
        assert_eq!(t.variants.len(), MAX_PALETTES);
        for (i, v) in t.variants.iter().enumerate() {
            let name = format!("{}-{}", i / 8 + 1, char::from(b'A' + (i % 8) as u8));
            assert_eq!(v.name, name);
            assert_eq!(v.id, name.to_lowercase().replace('-', "_"));
        }
        let Transform::Palette(first) = t.transform_for(0) else {
            panic!("palette")
        };
        assert_eq!(first.colours, [[31, 29, 25], [27, 18, 9], [21, 5, 4], [6, 3, 10]]);
        assert_eq!(first.output_max, 31.0);
        assert_eq!(t.transform_for(0), t.transform);
        // 4-H: 0x67FF = red 31, green 31, blue 25.
        let Transform::Palette(last) = t.transform_for(31) else {
            panic!("palette")
        };
        assert_eq!(last.colours[0], [31, 31, 25]);
        assert_ne!(t.transform_for(8), t.transform_for(0));
        // A number past the last palette gives the first one.
        assert_eq!(t.transform_for(99), t.transform);
        // The other kinds have no palettes and ignore the number.
        let gbc = for_value(crate::params::SOURCE_COLOUR_TARGET_GAME_BOY_COLOR).unwrap();
        assert!(gbc.variants.is_empty());
        assert_eq!(gbc.transform_for(5), gbc.transform);
    }

    /// The palette validation: a word of more than 15 bits, a repeated id, other than 5 bits per channel.
    #[test]
    fn a_bad_palette_file_is_refused() {
        let ok = SHIPPED[3].1.to_string();
        assert!(Target::parse("t.toml", &ok).is_ok());
        let cases = [
            (ok.replace("0x67BF", "0x87BF"), "0x7FFF"),
            (ok.replace("id = \"1_b\"", "id = \"1_a\""), "twice"),
            (ok.replace("colour_bits = 5", "colour_bits = 4"), "colour_bits"),
            (ok.replace("kind = \"palette\"", "kind = \"curve\""), "kind"),
            (ok.replace("sets = \"S:", "sets = \"X:"), "provenance.sets"),
        ];
        for (text, key) in cases {
            let err = Target::parse("t.toml", &text).expect_err(key);
            assert!(err.contains("t.toml") && err.contains(key), "{key}: {err}");
        }
    }

    /// The Mega Drive file: the measured levels and three emulator variants of known steps, the first one (variant 1,
    /// the default) Genesis Plus GX; the palette-style number picks one.
    #[test]
    fn the_mega_drive_file_holds_the_measured_levels_and_the_emulator_variants() {
        let t = for_value(SOURCE_COLOUR_TARGET_SEGA_MEGA_DRIVE).expect("md");
        let Transform::Dac(d) = t.transform else { panic!("dac") };
        assert_eq!(d.output, [0, 52, 87, 116, 144, 172, 206, 255]);
        assert_eq!(
            d.input,
            [0, 34, 68, 102, 136, 170, 204, 238],
            "the first variant is Genesis Plus GX"
        );
        let ids: Vec<&str> = t.variants.iter().map(|v| v.id.as_str()).collect();
        assert_eq!(ids, ["genesis_plus_gx", "kega_fusion", "regen"]);
        let names: Vec<&str> = t.variants.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, ["Genesis Plus GX", "Kega Fusion", "Regen"]);
        // The steps of the three emulators: 34, 33 and 32 per code.
        for (v, step) in t.variants.iter().zip([34u8, 33, 32]) {
            let Transform::Dac(d) = v.transform else { panic!("dac") };
            assert_eq!(d.input, std::array::from_fn(|i| step * i as u8), "{}", v.id);
        }
        // The measured levels come from TmEE's 15 mV levels, every second one, scaled so that 951 mV is 255.
        let mv: [f64; 8] = [0.0, 192.0, 324.0, 432.0, 537.0, 642.0, 768.0, 951.0];
        for (m, out) in mv.iter().zip(d.output) {
            assert!((m / 951.0 * 255.0 - f64::from(out)).abs() <= 0.6, "{m} mV is {out}");
        }
        // `select` takes the 1-based variant; 0, a negative number and a number past the last give the first.
        assert_eq!(t.select(1), t.variants[0].transform);
        assert_eq!(t.select(3), t.variants[2].transform);
        assert_eq!(t.select(0), t.variants[0].transform);
        assert_eq!(t.select(-4), t.variants[0].transform);
        assert_eq!(t.select(32), t.variants[0].transform);
        assert_ne!(t.select(2), t.select(3));
    }
    /// The dac validation: levels that do not increase, an output that decreases, a repeated id, no inputs.
    #[test]
    fn a_bad_dac_file_is_refused() {
        let ok = SHIPPED[4].1.to_string();
        assert!(Target::parse("t.toml", &ok).is_ok());
        let cases = [
            (
                ok.replace("levels = [0, 34, 68,", "levels = [0, 68, 34,"),
                "strictly increase",
            ),
            (
                ok.replace("output = [0, 52, 87,", "output = [0, 87, 52,"),
                "not decrease",
            ),
            (ok.replace("id = \"regen\"", "id = \"kega_fusion\""), "twice"),
            (ok.replace("output_max = 255", "output_max = 300"), "output_max"),
            (ok.replace("inputs = \"E:", "inputs = \"X:"), "provenance.inputs"),
        ];
        for (text, key) in cases {
            let err = Target::parse("t.toml", &text).expect_err(key);
            assert!(err.contains("t.toml") && err.contains(key), "{key}: {err}");
        }
    }

    /// The published C64 palettes (plain lists of 16 colours: Pepto 2001 and the Colodore defaults, which Lospec lists the
    /// same, and the more saturated Colodore file of VICE, compared only), each as bytes in the C64 colour order.
    const PEPTO: [u32; 16] = [
        0x000000, 0xFFFFFF, 0x68372B, 0x70A4B2, 0x6F3D86, 0x588D43, 0x352879, 0xB8C76F, 0x6F4F25, 0x433900, 0x9A6759,
        0x444444, 0x6C6C6C, 0x9AD284, 0x6C5EB5, 0x959595,
    ];
    const COLODORE: [u32; 16] = [
        0x000000, 0xFFFFFF, 0x813338, 0x75CEC8, 0x8E3C97, 0x56AC4D, 0x2E2C9B, 0xEDF171, 0x8E5029, 0x553800, 0xC46C71,
        0x4A4A4A, 0x7B7B7B, 0xA9FF9F, 0x706DEB, 0xB2B2B2,
    ];
    const VICE_COLODORE: [u32; 16] = [
        0x000000, 0xFFFFFF, 0x96282E, 0x5BD6CE, 0x9F2DAD, 0x41B936, 0x2724C4, 0xEFF347, 0x9F4815, 0x5E3500, 0xDA5F66,
        0x474747, 0x787878, 0x91FF84, 0x6864FF, 0xAEAEAE,
    ];

    fn rgb(x: u32) -> [f64; 3] {
        [(x >> 16) & 255, (x >> 8) & 255, x & 255].map(|c| f64::from(c) / 255.0)
    }

    /// The C64 file: four PAL revisions, 16 colours each, the first variant the 6569R3; the greys are grey, black and
    /// white are the rails, luma follows the measured order; and the classification works on the published palettes:
    /// every colour of Pepto's, Colodore's and VICE's Colodore palette is recognised as its own index on every
    /// revision, and the two closest hardware colours are farther apart than the threshold.
    #[test]
    fn the_c64_file_and_the_published_palettes() {
        let t = for_value(crate::params::SOURCE_COLOUR_TARGET_COMMODORE_64).expect("c64");
        let ids: Vec<&str> = t.variants.iter().map(|v| v.id.as_str()).collect();
        assert_eq!(ids, ["6569r3", "6569r4", "6569r5", "6569r1"]);
        for v in &t.variants {
            let Transform::Signal(s) = v.transform else {
                panic!("signal")
            };
            assert_eq!(s.count, 16);
            assert_eq!(s.colours[0], [0.0; 3], "{}: black", v.id);
            assert_eq!(s.colours[1], [1.0; 3], "{}: white", v.id);
            for i in [11, 12, 15] {
                let c = s.colours[i];
                assert!(
                    (c[0] - c[1]).abs() < 1e-9 && (c[1] - c[2]).abs() < 1e-9,
                    "{} grey {i}: {c:?}",
                    v.id
                );
            }
            // The greys rise: black, dark grey, medium grey, light grey, white (the measured luma order).
            let luma = |i: usize| 0.299 * s.colours[i][0] + 0.587 * s.colours[i][1] + 0.114 * s.colours[i][2];
            let order = [0, 11, 12, 15, 1];
            assert!(
                order.windows(2).all(|w| luma(w[0]) < luma(w[1])),
                "{}: grey order",
                v.id
            );
            // The two closest hardware colours, in the compared space, are farther apart than the threshold.
            let mut closest = f64::MAX;
            for i in 0..16 {
                for j in i + 1..16 {
                    let (a, b) = (s.reference(i), s.reference(j));
                    closest =
                        closest.min(((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt());
                }
            }
            println!(
                "{}: closest hardware colours {closest:.3}, threshold {}",
                v.id, s.threshold
            );
            // The reference colours are not closer than 0.1 (so that the nearest one is a safe choice); the threshold lies
            // beyond the farthest published palette (checked below), so it only leaves alone what is no palette colour.
            assert!(closest > 0.1, "{}: {closest}", v.id);
            for (name, pal) in [
                ("Pepto", PEPTO),
                ("Colodore", COLODORE),
                ("VICE Colodore", VICE_COLODORE),
            ] {
                let mut worst = 0.0f64;
                for (i, c) in pal.iter().enumerate() {
                    let got = s.classify(rgb(*c));
                    if got != Some(i) {
                        println!("  MISS {} {name}: colour {i} {c:06X} is {got:?}", v.id);
                    }
                    let r = s.reference(i);
                    let p = rgb(*c);
                    worst = worst.max(((p[0] - r[0]).powi(2) + (p[1] - r[1]).powi(2) + (p[2] - r[2]).powi(2)).sqrt());
                }
                println!("{} {name}: worst distance to its own colour {worst:.3}", v.id);
                assert!(worst <= s.threshold, "{} {name}: {worst}", v.id);
            }
        }
    }

    /// The R3 output colours are the decoder's drive voltages, hand derived for the red (colour 2): luma (1090 - 700) /
    /// (1850 - 700) = 0.33913; U = 0.15625 cos 101.25 = -0.030657, V = 0.15625 sin 101.25 = 0.153213; R = Y + V / 0.877283
    /// = 0.5138, B = Y + U / 0.492111 = 0.2768, G = (Y - 0.299 R - 0.114 B) / 0.587 = 0.2617.
    #[test]
    fn the_c64_output_is_the_decoders_drive_voltage() {
        let t = for_value(crate::params::SOURCE_COLOUR_TARGET_COMMODORE_64).expect("c64");
        let Transform::Signal(s) = t.transform else {
            panic!("signal")
        };
        let [r, g, b] = s.colours[2];
        assert!(
            (r - 0.5138).abs() < 1e-3 && (g - 0.2617).abs() < 1e-3 && (b - 0.2768).abs() < 1e-3,
            "{r} {g} {b}"
        );
        // A different chip, a different luma: the R4 red is darker (black 500 mV, red 840, white 1875).
        let Transform::Signal(r4) = t.transform_for(1) else {
            panic!("signal")
        };
        assert!(r4.colours[2][0] < r - 0.03, "{} {r}", r4.colours[2][0]);
        // The classification does not depend on the chip: the references are the same for every revision.
        let Transform::Signal(r1) = t.transform_for(3) else {
            panic!("signal")
        };
        assert_eq!(r4.references, s.references);
        assert_eq!(r1.references, s.references);
        // A recognised pixel is replaced by this chip's colour, an unknown pixel passes through.
        let pepto_red = rgb(PEPTO[2]);
        assert_eq!(s.apply(pepto_red), s.colours[2]);
        assert_ne!(s.apply(pepto_red), r4.apply(pepto_red));
        let odd = [0.2, 0.9, 0.95];
        assert_eq!(s.apply(odd), odd);
    }

    /// The 64 colours of published NES palettes, as emulators show them (plain lists: pally's two 2C02G palettes, MIT-0,
    /// made with its phase distortion filter, and the Mesen2 default, GPL, compared only; it equals Nestopia's canonical
    /// palette to within one level), hex RRGGBB in the order of the palette index.
    const PALLY_J: [&str; 64] = [
        "626262", "001C95", "1904AC", "42009D", "61006B", "6E0025", "650500", "491E00", "223700", "004900", "004F00",
        "004816", "00355E", "000000", "000000", "000000", "ABABAB", "0C4EDB", "3D2EFF", "7115F3", "9B0BB9", "B01262",
        "A92704", "894600", "576600", "237F00", "008900", "008332", "006D90", "000000", "000000", "000000", "FFFFFF",
        "57A5FF", "8287FF", "B46DFF", "DF60FF", "F863C6", "F8746D", "DE9020", "B3AE00", "81C800", "56D522", "3DD36F",
        "3EC1C8", "4E4E4E", "000000", "000000", "FFFFFF", "BEE0FF", "CDD4FF", "E0CAFF", "F1C4FF", "FCC4EF", "FDCACE",
        "F5D4AF", "E6DF9C", "D3E99A", "C2EFA8", "B7EFC4", "B6EAE5", "B8B8B8", "000000", "000000",
    ];
    const PALLY_U: [&str; 64] = [
        "575757", "000C8E", "0800A6", "340096", "550061", "630015", "5A0000", "3C0E00", "112800", "003B00", "004200",
        "003A05", "002652", "000000", "000000", "000000", "A5A5A5", "0041D9", "2F1EFF", "6704F2", "9400B4", "AA0057",
        "A31800", "803900", "4B5B00", "137600", "008100", "007923", "006288", "000000", "000000", "000000", "FFFFFF",
        "4A9FFF", "797EFF", "AF63FF", "DD55FF", "F757C2", "F76A63", "DC8810", "AEA900", "78C400", "4AD211", "2FCF64",
        "2FBDC4", "414141", "000000", "000000", "FFFFFF", "B9DDFF", "CAD1FF", "DEC6FF", "F0C0FF", "FCC0EE", "FDC6CA",
        "F5D0AA", "E4DD95", "D0E892", "BDEEA2", "B2EEC0", "B0E8E3", "B3B3B3", "000000", "000000",
    ];
    const MESEN: [&str; 64] = [
        "666666", "002A88", "1412A7", "3B00A4", "5C007E", "6E0040", "6C0600", "561D00", "333500", "0B4800", "005200",
        "004F08", "00404D", "000000", "000000", "000000", "ADADAD", "155FD9", "4240FF", "7527FE", "A01ACC", "B71E7B",
        "B53120", "994E00", "6B6D00", "388700", "0C9300", "008F32", "007C8D", "000000", "000000", "000000", "FFFEFF",
        "64B0FF", "9290FF", "C676FF", "F36AFF", "FE6ECC", "FE8170", "EA9E22", "BCBE00", "88D800", "5CE430", "45E082",
        "48CDDE", "4F4F4F", "000000", "000000", "FFFEFF", "C0DFFF", "D3D2FF", "E8C8FF", "FBC2FF", "FEC4EA", "FECCC5",
        "F7D8A5", "E4E594", "CFEF96", "BDF4AB", "B3F3CC", "B5EBF2", "B8B8B8", "000000", "000000",
    ];

    fn hex(s: &str) -> [f64; 3] {
        let v = u32::from_str_radix(s, 16).unwrap();
        [(v >> 16) & 255, (v >> 8) & 255, v & 255].map(|c| f64::from(c) / 255.0)
    }

    fn dist(a: [f64; 3], b: [f64; 3]) -> f64 {
        ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
    }

    /// The NES file: four variants of 64 colours, the first the 2C02G with the black at the blank level; hand-derived
    /// entries (the white, the black, two greys, and the yellow hue 8 of luma row 1): $18 has the level (312 + 840) / 2
    /// = 576 mV and the swing 528 mV, so Y = (576 - 312) / 788 = 0.33503 and the chroma 0.6440 x 528 / 788 = 0.43151 at
    /// 30 x 6 - 5 = 175 degrees: U = -0.42987, V = 0.037608, R = Y + V / 0.877283 = 0.3779, B = Y + U / 0.492111 = -0.5385
    /// (clamped to 0 after the decode), G = (Y - 0.299 R - 0.114 B) / 0.587 = 0.4833 (the green is decoded from the unclamped red and blue, so the luma is kept).
    #[test]
    fn the_nes_file_and_its_hand_derived_colours() {
        let t = for_value(crate::params::SOURCE_COLOUR_TARGET_NINTENDO_ENTERTAINMENT_SYSTEM).expect("nes");
        let ids: Vec<&str> = t.variants.iter().map(|v| v.id.as_str()).collect();
        assert_eq!(ids, ["2c02g_blank", "2c02g_setup", "2c02e_blank", "2c02e_setup"]);
        let Transform::Signal(s) = t.transform else {
            panic!("signal")
        };
        assert_eq!(s.count, 64);
        assert_eq!(s.colours[0x20], [1.0; 3], "$20 is white");
        assert_eq!(s.colours[0x0F], [0.0; 3], "$0F is black");
        assert_eq!(s.colours[0x0D], [0.0; 3], "$0D is blacker than black, clamped");
        let grey = |i: usize, y: f64| {
            assert!(
                s.colours[i].iter().all(|c| (c - y).abs() < 1e-9),
                "{i:#X}: {:?}",
                s.colours[i]
            )
        };
        grey(0x2D, (552.0 - 312.0) / 788.0);
        grey(0x3D, (880.0 - 312.0) / 788.0);
        grey(0x00, (616.0 - 312.0) / 788.0);
        let [r, g, b] = s.colours[0x18];
        assert!(
            (r - 0.3779).abs() < 1e-3 && (g - 0.4833).abs() < 1e-3 && b == 0.0,
            "{r} {g} {b}"
        );
        // The setup variant is darker (black 365.6 mV): the grey of $2D falls to (552 - 365.6) / 734.4.
        let Transform::Signal(u) = t.transform_for(1) else {
            panic!("signal")
        };
        assert!((u.colours[0x2D][0] - (552.0 - 365.6) / 734.4).abs() < 1e-9);
        // The 2C02E turns less per row than the 2C02G: the colours of row 3 differ, the colours of row 0 do not.
        let Transform::Signal(e) = t.transform_for(2) else {
            panic!("signal")
        };
        assert_eq!(e.colours[0x01], s.colours[0x01]);
        assert_ne!(e.colours[0x31], s.colours[0x31]);
        // The references are the same for every variant.
        assert_eq!(e.references, s.references);
        assert_eq!(u.references, s.references);
    }

    /// The model against pally's 2C02G palettes (made with its phase distortion filter, which also lowers the chroma):
    /// the mean distance in drive voltage stays under 0.1 (it is about 0.07), for Japan against the first variant and for
    /// the United States against the setup one.
    #[test]
    fn the_nes_model_is_near_the_pally_2c02g_palettes() {
        let t = for_value(crate::params::SOURCE_COLOUR_TARGET_NINTENDO_ENTERTAINMENT_SYSTEM).expect("nes");
        for (variant, pal, name) in [(0, &PALLY_J, "pally J"), (1, &PALLY_U, "pally U")] {
            let Transform::Signal(s) = t.transform_for(variant) else {
                panic!("signal")
            };
            let mean = (0..64).map(|i| dist(hex(pal[i]), s.colours[i])).sum::<f64>() / 64.0;
            println!("{name}: mean distance to the model {mean:.3}");
            assert!(mean < 0.1, "{name}: {mean}");
        }
    }

    /// Recognition on the palettes of the emulators that decode the signal: every entry but at most 3 is recognised as a
    /// colour whose hardware colour is the expected one (the duplicated blacks of a palette are one colour), within the
    /// threshold.
    #[test]
    fn the_nes_palettes_of_the_emulators_are_recognised() {
        let t = for_value(crate::params::SOURCE_COLOUR_TARGET_NINTENDO_ENTERTAINMENT_SYSTEM).expect("nes");
        let Transform::Signal(s) = t.transform else {
            panic!("signal")
        };
        for (name, pal) in [("pally J", &PALLY_J), ("pally U", &PALLY_U), ("Mesen2", &MESEN)] {
            let (mut worst, mut wrong) = (0.0f64, Vec::new());
            for (i, entry) in pal.iter().enumerate() {
                let p = hex(entry);
                worst = worst.max(dist(p, s.references[i]));
                match s.classify(p) {
                    Some(j) if dist(s.colours[j], s.colours[i]) < 0.02 => {}
                    got => wrong.push((i, got)),
                }
            }
            println!(
                "{name}: worst distance to its own reference {worst:.3}, {} wrong",
                wrong.len()
            );
            // A few dark greens and blues of row 0 sit between two references: at most 3 of the 64 are confused.
            assert!(wrong.len() <= 3, "{name}: {wrong:?}");
            assert!(worst <= s.threshold, "{name}: {worst}");
        }
    }

    /// The CGA remap: the canonical colours (and the dark yellow of a monitor without the brown circuit) are replaced by
    /// the colours Holden measured on the IBM 5153. Hand derivation: the brown of the canonical palette, AA5500, is the
    /// index 6, whose gun voltages are 1.00, 0.64 and 0 V over 1.30 V: (196, 126, 0) of 255; the intensified white is
    /// 1.30 V, 255; the dark grey of intensity 0.40 V is 78 of 255.
    #[test]
    fn the_cga_file_replaces_the_canonical_colours_by_the_measured_ones() {
        let t = for_value(crate::params::SOURCE_COLOUR_TARGET_IBM_CGA_RGBI).expect("cga");
        let ids: Vec<&str> = t.variants.iter().map(|v| v.id.as_str()).collect();
        assert_eq!(ids, ["ibm_5153_max_contrast"]);
        let Transform::Signal(s) = t.transform else {
            panic!("signal")
        };
        assert_eq!(s.count, 17, "16 canonical colours and the dark yellow");
        let b = |v: f64| (v * 255.0).round() as u32;
        let hexes = |c: [f64; 3]| (b(c[0]), b(c[1]), b(c[2]));
        // The canonical brown AA5500 shows C47E00; so does the dark yellow AAAA00 (colour 6 again).
        let brown = hex_rgb(0xAA5500);
        assert_eq!(hexes(s.apply(brown)), (0xC4, 0x7E, 0x00));
        assert_eq!(hexes(s.apply(hex_rgb(0xAAAA00))), (0xC4, 0x7E, 0x00));
        assert_eq!(hexes(s.apply(hex_rgb(0xFFFFFF))), (0xFF, 0xFF, 0xFF));
        assert_eq!(hexes(s.apply(hex_rgb(0x555555))), (0x4E, 0x4E, 0x4E));
        assert_eq!(
            hexes(s.apply(hex_rgb(0x0000AA))),
            (0x00, 0x00, 0xC4),
            "the blue of the normal half is 1.00 V"
        );
        assert_eq!(
            hexes(s.apply(hex_rgb(0x55FFFF))),
            (0x4E, 0xF3, 0xF3),
            "the light cyan: 0.40, 1.24, 1.24 V"
        );
        // Every canonical colour is recognised as its own index, and the 5153's colours are those of the voltage table
        // over 1.30 V (the volts of Holden's table, as the file derives them).
        let volts: [[f64; 3]; 16] = [
            [0.0, 0.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.0, 1.0, 0.0],
            [0.0, 1.0, 1.0],
            [1.0, 0.0, 0.0],
            [1.0, 0.0, 1.0],
            [1.0, 0.64, 0.0],
            [1.0, 1.0, 1.0],
            [0.4, 0.4, 0.4],
            [0.4, 0.4, 1.12],
            [0.4, 1.12, 0.4],
            [0.4, 1.24, 1.24],
            [1.12, 0.4, 0.4],
            [1.24, 0.4, 1.24],
            [1.24, 1.24, 0.4],
            [1.3, 1.3, 1.3],
        ];
        for (i, v) in volts.iter().enumerate() {
            assert_eq!(s.classify(s.reference(i)), Some(i));
            let out = s.colours[i];
            for c in 0..3 {
                assert!(
                    (out[c] - v[c] / 1.3).abs() < 0.003,
                    "colour {i} channel {c}: {} vs {}",
                    out[c],
                    v[c] / 1.3
                );
            }
        }
        // A pixel far from every canonical colour passes through.
        let odd = [0.2, 0.45, 0.15];
        assert_eq!(s.apply(odd), odd);
    }

    fn hex_rgb(c: u32) -> [f64; 3] {
        [(c >> 16) & 255, (c >> 8) & 255, c & 255].map(|v| f64::from(v) / 255.0)
    }

    /// The remap validation: an output of the wrong length, a reference past 0xFFFFFF, a repeated id, a missing provenance.
    #[test]
    fn a_bad_remap_file_is_refused() {
        let ok = include_str!("../../../presets/targets/ibm_cga_rgbi.toml").to_string();
        assert!(Target::parse("t.toml", &ok).is_ok());
        let cases = [
            (ok.replace("rgb = 0xFFFFFF", "rgb = 0x1FFFFFF"), "0xFFFFFF"),
            (
                ok.replace("output = [0x000000,", "output = [0x000001, 0x000000,"),
                "output",
            ),
            (ok.replace("threshold = 0.1", "threshold = 0"), "threshold"),
            (ok.replace("variants = \"S:", "variants = \"X:"), "provenance.variants"),
            (ok.replace("kind = \"remap\"", "kind = \"signal\""), "kind"),
        ];
        for (text, key) in cases {
            let err = Target::parse("t.toml", &text).expect_err(key);
            assert!(err.contains("t.toml") && err.contains(key), "{key}: {err}");
        }
    }

    /// The Pokefan531 Game Boy Color: the libretro matrix (rows summing to 1, lum 0.94) and the gamma step. Hand
    /// derivation: a pure red (code 31) with the exponent 1.2 gives linear (1, 0, 0), R = 0.82 x 0.94 = 0.7708, G = 0.125 x
    /// 0.94 = 0.1175, B = 0.195 x 0.94 = 0.1833, each raised to 1 / 2.2: 0.88838, 0.37777, 0.46255; a pure blue has a
    /// negative red weight (-0.06), which clamps to 0; white (1, 1, 1) is 0.94 on every row, 0.94 ^ (1 / 2.2) = 0.97226; a grey
    /// code 16 with the central exponent 1.7: (16 / 31) ^ 1.7 = 0.32492, x 0.94 = 0.30543, ^ (1 / 2.2) = 0.58310.
    #[test]
    fn the_pokefan531_gbc_file_follows_the_libretro_shader() {
        let t = for_value(crate::params::SOURCE_COLOUR_TARGET_GAME_BOY_COLOR_COLORIMETRIC).expect("gbc colorimetric");
        let ids: Vec<&str> = t.variants.iter().map(|v| v.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "libretro_light_above",
                "gambatte_light_central",
                "light_below",
                "mgba_srgb"
            ]
        );
        let code = |c: u32| f64::from((c << 3) | (c >> 2)) / 255.0;
        let near = |a: f64, b: f64| (a - b).abs() < 1e-3;
        let [r, g, b] = t.transform_for(0).apply([1.0, 0.0, 0.0]);
        assert!(near(r, 0.88838) && near(g, 0.37777) && near(b, 0.46255), "{r} {g} {b}");
        let [r, _, b] = t.transform_for(0).apply([0.0, 0.0, 1.0]);
        assert!(
            r == 0.0 && near(b, (0.73f64 * 0.94).powf(1.0 / 2.2)),
            "negative weight clamped: {r} {b}"
        );
        for c in t.transform_for(0).apply([1.0; 3]) {
            assert!(near(c, 0.97226), "{c}");
        }
        let grey = t.transform_for(1).apply([code(16); 3]);
        assert!(grey.iter().all(|c| near(*c, 0.58310)), "{grey:?}");
        // The exponents: 1.2, 1.7, 2.2 darken the mid grey more and more.
        let mids: Vec<f64> = (0..3).map(|i| t.transform_for(i).apply([code(16); 3])[0]).collect();
        assert!(mids[0] > mids[1] && mids[1] > mids[2], "{mids:?}");
        // mGBA's sRGB profile: white is lum 0.91, 0.91 ^ (1 / 2.2) = 0.95810.
        for c in t.transform_for(3).apply([1.0; 3]) {
            assert!(near(c, 0.95810), "{c}");
        }
    }

    /// The AGS-101 target against an independent computation: the matrices of the variants are recomputed here from the XYZ
    /// readings (the normalised primary matrix times the XYZ to linear sRGB matrix, and the Bradford adaptation to D65), the
    /// exponents from the 32-step grey ramps (a logarithmic least-squares fit of the black-subtracted ramp over the steps 4
    /// to 28), and the white of the D65 variants is neutral, the white as measured is full drive on its brightest channel.
    #[test]
    #[allow(clippy::needless_range_loop)]
    fn the_ags_101_file_is_recomputed_from_the_readings() {
        type M = [[f64; 3]; 3];
        let mul = |a: &M, b: &M| {
            let mut r = [[0.0; 3]; 3];
            for i in 0..3 {
                for j in 0..3 {
                    for k in 0..3 {
                        r[i][j] += a[i][k] * b[k][j];
                    }
                }
            }
            r
        };
        let mulv = |a: &M, v: [f64; 3]| [0, 1, 2].map(|i| a[i][0] * v[0] + a[i][1] * v[1] + a[i][2] * v[2]);
        let inv = |m: &M| {
            let [[a, b, c], [d, e, f], [g, h, i]] = *m;
            let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
            [
                [(e * i - f * h) / det, (c * h - b * i) / det, (b * f - c * e) / det],
                [(f * g - d * i) / det, (a * i - c * g) / det, (c * d - a * f) / det],
                [(d * h - e * g) / det, (b * g - a * h) / det, (a * e - b * d) / det],
            ]
        };
        let diag = |v: [f64; 3]| [[v[0], 0.0, 0.0], [0.0, v[1], 0.0], [0.0, 0.0, v[2]]];
        let to_srgb: M = [
            [3.2406255, -1.5372080, -0.4986286],
            [-0.9689307, 1.8757561, 0.0415175],
            [0.0557101, -0.2040211, 1.0569959],
        ];
        let bradford: M = [
            [0.8951, 0.2664, -0.1614],
            [-0.7502, 1.7135, 0.0367],
            [0.0389, -0.0685, 1.0296],
        ];
        let d65 = [0.95047, 1.0, 1.08883];
        // (red, green, blue, white) XYZ of the two sessions, and the 32 grey steps (Y) of each.
        let sessions: [([[f64; 3]; 4], [f64; 32]); 2] = [
            (
                [
                    [53.035133, 29.786356, 3.376642],
                    [43.2085, 79.048882, 8.263728],
                    [25.650356, 15.12584, 136.981308],
                    [121.178963, 122.963692, 151.95961],
                ],
                [
                    0.476724, 0.57227, 0.730655, 0.928297, 1.300691, 1.972238, 3.104219, 4.684548, 6.752009, 8.528899,
                    10.644724, 13.507071, 16.091734, 18.133703, 20.990744, 23.672119, 26.548615, 30.127653, 33.071106,
                    36.36824, 40.365112, 45.727238, 50.626457, 57.723812, 63.798332, 71.693527, 77.374222, 85.518791,
                    95.451256, 106.659615, 116.605713, 122.963692,
                ],
            ),
            (
                [
                    [55.509378, 31.260651, 2.961776],
                    [43.927962, 80.674025, 8.561921],
                    [26.129149, 15.251464, 139.940875],
                    [121.795317, 123.381279, 149.195392],
                ],
                [
                    0.391119, 0.474823, 0.594589, 0.763017, 1.069079, 1.636336, 2.566728, 4.062557, 5.803986, 7.413745,
                    9.4599, 12.004312, 14.511519, 16.591394, 18.977837, 21.629573, 24.58209, 27.647328, 30.921221,
                    34.6192, 38.844053, 43.804122, 49.010497, 54.66813, 61.273021, 69.030122, 76.815933, 84.59095,
                    95.189857, 107.539479, 116.89763, 123.381279,
                ],
            ),
        ];
        let t = for_value(crate::params::SOURCE_COLOUR_TARGET_GAME_BOY_ADVANCE_SP_AGS_101).expect("ags");
        for (s, (xyz, ramp)) in sessions.iter().enumerate() {
            let chroma = |p: [f64; 3]| [p[0] / p[1], 1.0, p[2] / p[1]];
            let (cr, cg, cb, w) = (chroma(xyz[0]), chroma(xyz[1]), chroma(xyz[2]), chroma(xyz[3]));
            let p: M = [[cr[0], cg[0], cb[0]], [1.0, 1.0, 1.0], [cr[2], cg[2], cb[2]]];
            let m = mul(&p, &diag(mulv(&inv(&p), w)));
            let measured = mul(&to_srgb, &m);
            let (rs, rd) = (mulv(&bradford, w), mulv(&bradford, d65));
            let cat = mul(
                &inv(&bradford),
                &mul(&diag([rd[0] / rs[0], rd[1] / rs[1], rd[2] / rs[2]]), &bradford),
            );
            let adapted = mul(&to_srgb, &mul(&cat, &m));
            // The exponent: log-log least squares of the black-subtracted ramp over the steps 4 to 28.
            let (black, white) = (ramp[0], ramp[31] - ramp[0]);
            let pts: Vec<(f64, f64)> = (4..=28)
                .map(|i| ((i as f64 / 31.0).ln(), ((ramp[i] - black) / white).ln()))
                .collect();
            let n = pts.len() as f64;
            let (sx, sy) = pts.iter().fold((0.0, 0.0), |a, p| (a.0 + p.0, a.1 + p.1));
            let (sxx, sxy) = pts.iter().fold((0.0, 0.0), |a, p| (a.0 + p.0 * p.0, a.1 + p.0 * p.1));
            let gamma = (n * sxy - sx * sy) / (n * sxx - sx * sx);
            for (k, (rows, label)) in [(measured, "measured white"), (adapted, "D65 white")]
                .into_iter()
                .enumerate()
            {
                let Transform::Matrix(f) = t.transform_for(s * 2 + k) else {
                    panic!("matrix")
                };
                for r in 0..3 {
                    for c in 0..3 {
                        assert!(
                            (f.rows[r][c] - rows[r][c]).abs() < 2e-9,
                            "session {s} {label} row {r} col {c}: {} vs {}",
                            f.rows[r][c],
                            rows[r][c]
                        );
                    }
                }
                assert!(
                    (f.input_gamma - gamma).abs() < 0.01,
                    "session {s}: exponent {} vs fitted {gamma}",
                    f.input_gamma
                );
                // The white: neutral for the D65 variants, the brightest channel at full drive for the measured one.
                let out = Transform::Matrix(f).apply([1.0; 3]);
                let top = out.iter().cloned().fold(0.0, f64::max);
                if k == 0 {
                    assert!(
                        (top - 1.0).abs() < 2e-3,
                        "session {s}: the brightest channel of the white {top}"
                    );
                } else {
                    assert!(
                        out.iter().all(|c| (c - out[0]).abs() < 0.01),
                        "session {s}: white {out:?}"
                    );
                }
            }
        }
    }
}
