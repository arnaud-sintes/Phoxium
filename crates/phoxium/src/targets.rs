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
//! The `signal` kind (C64, NES, Atari 8-bit; `[signal]`, `[signal.reference]`, `[[signal.revisions]]`) lists its colours
//! one by one (`[[signal.colours]]`) or, for a palette that is a grid of hues and luma levels (the Atari's 256), as
//! `[signal.grid]` (`hues`, `lumas`, the phase `angle` of hue 1): the colour of hue `h` and luma `l` has the index
//! `h x lumas + l`, hue 0 has no chroma, the phase of hue `h` is `angle + phase_row_deg x (h - 1)` and the `luma_mv` of
//! a revision or of the reference holds one level per luma. A revision may give its own chroma `amplitude`. The reference
//! the classification compares with is computed from the colours (`[signal.reference]`), or, with `rgb_references`, a
//! list of the RGB colours an emulator shows (the CGA's canonical RGBI palette).
//!
//! [`Target::parse`] rejects an unknown key, a schema other than 1, an unknown kind, a table of the other kind, a value
//! that is not finite or not in range, and a key with no provenance entry of the form `<tag>: <text>`. A target ships
//! only when the source of its values can be named (QUESTIONS.md, Study finding 2).

use std::{collections::BTreeMap, sync::OnceLock};

use crt_model::target::{
    decode_yuv, Curve, Dac, Matrix, Palette, Signal, Transform, CURVE_LEN, DAC_LEN, SIGNAL_MAX, U_PER_B_Y, V_PER_R_Y,
};
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
    (
        "atari_8bit_gtia.toml",
        include_str!("../../../presets/targets/atari_8bit_gtia.toml"),
    ),
    (
        "texas_instruments_tms9928.toml",
        include_str!("../../../presets/targets/texas_instruments_tms9928.toml"),
    ),
    (
        "snk_neo_geo.toml",
        include_str!("../../../presets/targets/snk_neo_geo.toml"),
    ),
    (
        "sega_master_system.toml",
        include_str!("../../../presets/targets/sega_master_system.toml"),
    ),
    (
        "ibm_cga_composite.toml",
        include_str!("../../../presets/targets/ibm_cga_composite.toml"),
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
    /// Variants of the curve, each a factor on the whole output (the Neo Geo's dark bit and shadow); the first one is the
    /// target's own.
    variants: Option<Vec<CurveVariant>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CurveVariant {
    id: String,
    name: String,
    /// The factor on every output level, above 0 and at most 1.
    scale: f64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DacFile {
    /// The output code of each code (2 to 8 codes), the red channel's and, unless a variant gives its own, the three
    /// channels'.
    output: Vec<u32>,
    output_max: f64,
    inputs: Vec<DacInput>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DacInput {
    id: String,
    name: String,
    /// The 8-bit level the emulator shows for each code (as many as `output`).
    levels: Vec<u32>,
    /// This variant's own output codes, red (and green and blue unless they are given): the Master System's two VDPs
    /// differ in their output. A variant without it takes the file's `output` for the three channels.
    output: Option<Vec<u32>>,
    /// The green channel's own codes, only with `output`.
    output_green: Option<Vec<u32>>,
    /// The blue channel's own codes, only with `output`.
    output_blue: Option<Vec<u32>>,
}

/// The codes of a `dac` table as the arrays of [`Dac`]: 2 to 8 values of at most 255, the last one repeated to fill the 8.
fn dac_codes(file: &str, key: &str, v: &[u32], count: Option<usize>) -> Result<[u8; DAC_LEN], String> {
    if v.len() < 2 || v.len() > DAC_LEN || count.is_some_and(|n| n != v.len()) {
        return Err(format!(
            "{file}: dac.{key} must hold {} entries, it has {}",
            count.map_or("2 to 8".to_string(), |n| n.to_string()),
            v.len()
        ));
    }
    let mut out = [0u8; DAC_LEN];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::try_from(v[i.min(v.len() - 1)]).map_err(|_| format!("{file}: dac.{key} entries must be 0 to 255"))?;
    }
    Ok(out)
}

impl DacFile {
    /// Whether a variant has output codes of its own (then the provenance holds `variant_outputs`).
    fn has_variant_outputs(&self) -> bool {
        self.inputs
            .iter()
            .any(|s| s.output.is_some() || s.output_green.is_some() || s.output_blue.is_some())
    }

    /// The keys that are present, for the provenance check.
    fn keys(&self) -> Vec<&'static str> {
        let mut k = vec!["output", "output_max", "inputs"];
        if self.has_variant_outputs() {
            k.push("variant_outputs");
        }
        k
    }

    /// The emulator inputs as variants, the first one the default.
    fn variants(&self, file: &str) -> Result<Vec<Variant>, String> {
        positive(file, "dac", "output_max", self.output_max)?;
        if self.output_max > 255.0 {
            return Err(format!("{file}: dac.output_max must be at most 255"));
        }
        let count = self.output.len();
        let output = dac_codes(file, "output", &self.output, None)?;
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
            let input = dac_codes(file, &format!("inputs {} levels", s.id), &s.levels, Some(count))?;
            if input[..count].windows(2).any(|w| w[0] >= w[1]) {
                return Err(format!("{file}: dac.inputs {} levels must strictly increase", s.id));
            }
            if s.output.is_none() && (s.output_green.is_some() || s.output_blue.is_some()) {
                return Err(format!(
                    "{file}: dac.inputs {} output_green and output_blue need its own output",
                    s.id
                ));
            }
            let own = |key: &str, v: &Option<Vec<u32>>| -> Result<Option<[u8; DAC_LEN]>, String> {
                let Some(v) = v else {
                    return Ok(None);
                };
                let a = dac_codes(file, &format!("inputs {} {key}", s.id), v, Some(count))?;
                if a.windows(2).any(|w| w[0] > w[1]) {
                    return Err(format!("{file}: dac.inputs {} {key} must not decrease", s.id));
                }
                Ok(Some(a))
            };
            let red = own("output", &s.output)?.unwrap_or(output);
            out.push(Variant {
                id: s.id.clone(),
                name: s.name.clone(),
                transform: Transform::Dac(Dac {
                    input,
                    output: red,
                    output_green: own("output_green", &s.output_green)?,
                    output_blue: own("output_blue", &s.output_blue)?,
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
    /// The level, in the units of the luma levels, at which a colour difference (`r_y`, `b_y`) is zero (the TMS9928A's
    /// datasheet gives its differences around 0.47 of the white level); 0 when absent.
    neutral: Option<f64>,
    threshold: f64,
    /// The colours whose luma is the black and the white level, for a revision that does not give `black_mv` and
    /// `white_mv`.
    black_index: Option<usize>,
    white_index: Option<usize>,
    /// The model of the reference palette the classification compares with.
    reference: SignalReference,
    /// The colours one by one (C64, NES), or none when `grid` makes them.
    #[serde(default)]
    colours: Vec<SignalColour>,
    /// The colours as a grid of hues and luma levels (Atari 8-bit): an alternative to `colours`.
    grid: Option<SignalGrid>,
    revisions: Vec<SignalRevision>,
}

/// A palette of `hues` x `lumas` colours, the colour of hue `h` and luma `l` at the index `h x lumas + l` (the value of an
/// Atari colour register: hue in the high nibble, luma in the low one). Hue 0 has no chroma; hue `h` of 1 and up has the
/// phase `angle + phase_row_deg x (h - 1)` degrees on the U axis, the `phase_row_deg` being the turn of the revision (one
/// delay of the hue chain), and the level of a colour is the `luma_mv` entry of its luma, the same for every hue (so the
/// `luma_mv` lists of the revisions and of the reference hold `lumas` entries).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SignalGrid {
    hues: usize,
    lumas: usize,
    /// The phase in degrees of hue 1, the first one with a chroma.
    angle: f64,
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
    /// Further exponents: the reference palette is made once at `gamma` and once at each of these, and every set is
    /// compared (the colour `i` of the set `k` is the table entry `k x colours + i`, and gives the colour `i`). Emulators
    /// show the same chip with different display gammas (the TMS9928A palettes go from 1.0 to 1.6), and one exponent
    /// cannot sit near them all.
    extra_gammas: Option<Vec<f64>>,
    /// The reference as a list of colours an emulator shows (0xRRGGBB) with the index of the colour each stands for
    /// (several may stand for one colour), in place of a computed palette: for a device whose emulators show a fixed RGB
    /// palette that is not the signal's decode (the IBM CGA's canonical RGBI colours). It excludes every key that
    /// computes the reference (`luma_mv`, `black_mv`, `white_mv`, `phase_row_deg`, `amplitude`, `saturation`,
    /// `extra_gammas`) and needs a `gain` and a `gamma` of 1.
    rgb_references: Option<Vec<RemapReference>>,
}

#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct SignalColour {
    name: String,
    /// The chroma phase in degrees on the U axis (U = cos, V = sin); none for a grey.
    angle: Option<f64>,
    /// The row of the colour (the luma row 0 to 3 of the NES palette, the hue minus 1 of a grid), which turns the phase by
    /// `phase_row_deg` degrees a row.
    row: Option<u32>,
    /// The level of the colour in mV (the mean of a square wave, the constant level of a grey), when the revision gives no
    /// `luma_mv`.
    level_mv: Option<f64>,
    /// The peak-to-peak swing in mV of the square wave that makes the chroma.
    swing_mv: Option<f64>,
    /// The colour differences R' - Y and B' - Y as the chip gives them (the TMS9928A), in the units of the levels and around
    /// `neutral`, when the revision gives no `r_y` and `b_y` lists: an alternative to `angle`.
    r_y: Option<f64>,
    b_y: Option<f64>,
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
    /// The chroma amplitude of this revision, replacing the file's `amplitude` (the Atari machines each have their own).
    amplitude: Option<f64>,
    /// The colour differences of each colour, in the order of `colours` (replace the colours' `r_y` and `b_y`), and their
    /// zero level (replaces the file's `neutral`).
    r_y: Option<Vec<f64>>,
    b_y: Option<Vec<f64>>,
    neutral: Option<f64>,
}

/// The settings that turn the colours of a signal file into drive voltages (a revision, or the reference model).
struct SignalSettings<'a> {
    luma_mv: Option<&'a [f64]>,
    r_y: Option<&'a [f64]>,
    b_y: Option<&'a [f64]>,
    neutral: Option<f64>,
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
        let mut k = vec![
            if self.grid.is_some() { "grid" } else { "colours" },
            "revisions",
            "reference",
            "threshold",
        ];
        for (name, present) in [
            ("amplitude", self.amplitude.is_some()),
            ("chroma_factor", self.chroma_factor.is_some()),
            ("neutral", self.neutral.is_some()),
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
    fn colours_for(
        &self,
        file: &str,
        what: &str,
        colours: &[SignalColour],
        s: &SignalSettings,
    ) -> Result<[[f64; 3]; SIGNAL_MAX], String> {
        let n = colours.len();
        let level = |i: usize| -> Result<f64, String> {
            s.luma_mv
                .map(|l| l[i])
                .or(colours[i].level_mv)
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
        let neutral = s.neutral.or(self.neutral).unwrap_or(0.0);
        let mut out = [[0.0; 3]; SIGNAL_MAX];
        for (i, c) in colours.iter().enumerate() {
            let y = (level(i)? - black) / span;
            let amplitude = match (c.swing_mv, self.chroma_factor, s.amplitude.or(self.amplitude)) {
                (Some(sw), Some(k), _) => k * sw / span,
                (_, _, Some(a)) => a,
                _ => 0.0,
            };
            // Colour differences given by the chip: R' - Y and B' - Y on the scale of the luma, scaled to U and V.
            let differences = match (s.r_y.map(|l| l[i]).or(c.r_y), s.b_y.map(|l| l[i]).or(c.b_y)) {
                (Some(ry), Some(by)) => Some((U_PER_B_Y * (by - neutral) / span, V_PER_R_Y * (ry - neutral) / span)),
                _ => None,
            };
            let (u, v) = differences.unwrap_or_else(|| {
                c.angle.map_or((0.0, 0.0), |a| {
                    let a = (a + s.phase_row_deg * f64::from(c.row.unwrap_or(0))).to_radians();
                    (amplitude * a.cos(), amplitude * a.sin())
                })
            });
            out[i] =
                decode_yuv(y * s.gain, u * s.gain * s.saturation, v * s.gain * s.saturation).map(|c| c.powf(s.gamma));
        }
        Ok(out)
    }

    /// The colours of the file: the list, or the grid of hues and luma levels expanded (index = hue x lumas + luma).
    fn colour_list(&self, file: &str) -> Result<Vec<SignalColour>, String> {
        let Some(g) = &self.grid else {
            return Ok(self.colours.clone());
        };
        if !self.colours.is_empty() {
            return Err(format!(
                "{file}: signal.grid and signal.colours are alternatives, not both"
            ));
        }
        if g.hues < 2 || g.lumas < 2 || g.hues.saturating_mul(g.lumas) > SIGNAL_MAX || !g.angle.is_finite() {
            return Err(format!(
                "{file}: signal.grid needs 2 or more hues and lumas, at most {SIGNAL_MAX} colours, a finite angle"
            ));
        }
        Ok((0..g.hues * g.lumas)
            .map(|i| {
                let (hue, luma) = (i / g.lumas, i % g.lumas);
                SignalColour {
                    name: format!("hue {hue} luma {luma}"),
                    angle: (hue > 0).then_some(g.angle),
                    row: (hue > 0).then(|| (hue - 1) as u32),
                    level_mv: None,
                    swing_mv: None,
                    r_y: None,
                    b_y: None,
                }
            })
            .collect())
    }

    /// A list of levels for the colours: one per colour, or (with a grid) one per luma, the same for every hue.
    fn levels(&self, list: &[f64], n: usize) -> Option<Vec<f64>> {
        match &self.grid {
            Some(g) if list.len() == g.lumas => Some((0..n).map(|i| list[i % g.lumas]).collect()),
            _ if list.len() == n => Some(list.to_vec()),
            _ => None,
        }
    }

    /// The revisions as variants: each colour's drive voltages from its level, chroma angle and amplitude, and the
    /// reference palette of the classification.
    fn variants(&self, file: &str) -> Result<Vec<Variant>, String> {
        let list = self.colour_list(file)?;
        let n = list.len();
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
        let extra_gammas = r.extra_gammas.clone().unwrap_or_default();
        for g in &extra_gammas {
            positive(file, "signal.reference", "extra_gammas", *g)?;
        }
        let rgb_refs = r.rgb_references.as_deref();
        if let Some(refs) = rgb_refs {
            if refs.is_empty() || refs.len() > SIGNAL_MAX {
                return Err(format!(
                    "{file}: signal.reference.rgb_references must hold 1 to {SIGNAL_MAX} colours"
                ));
            }
            if refs.iter().any(|x| x.rgb > 0xFF_FFFF || x.colour >= n) {
                return Err(format!(
                    "{file}: signal.reference.rgb_references rgb must be at most 0xFFFFFF and colour one of the {n} colours"
                ));
            }
            if r.luma_mv.is_some()
                || r.black_mv.is_some()
                || r.white_mv.is_some()
                || r.phase_row_deg.is_some()
                || r.amplitude.is_some()
                || r.saturation.is_some()
                || !extra_gammas.is_empty()
                || r.gain != 1.0
                || r.gamma != 1.0
            {
                return Err(format!(
                    "{file}: signal.reference.rgb_references replaces the computed reference: no luma_mv, black_mv, white_mv, \
                     phase_row_deg, amplitude, saturation or extra_gammas, and a gain and a gamma of 1"
                ));
            }
        }
        let sets = if rgb_refs.is_some() { 1 } else { 1 + extra_gammas.len() };
        if n.saturating_mul(sets) > SIGNAL_MAX {
            return Err(format!(
                "{file}: {n} colours in {sets} reference sets are more than the {SIGNAL_MAX} the table holds"
            ));
        }
        if let Some(v) = self.neutral {
            if !v.is_finite() {
                return Err(format!("{file}: signal.neutral must be finite"));
            }
        }
        let ref_luma = match &r.luma_mv {
            None => None,
            Some(l) => Some(
                self.levels(l, n)
                    .filter(|l| l.iter().all(|v| v.is_finite() && *v >= 0.0))
                    .ok_or_else(|| {
                        format!("{file}: signal.reference.luma_mv must hold {n} levels (a grid: one per luma), not negative")
                    })?,
            ),
        };
        if list.iter().any(|c| c.name.trim().is_empty()) {
            return Err(format!("{file}: signal.colours name must not be empty"));
        }
        if list.iter().any(|c| {
            [c.angle, c.level_mv, c.swing_mv, c.r_y, c.b_y]
                .iter()
                .flatten()
                .any(|v| !v.is_finite())
        }) {
            return Err(format!(
                "{file}: signal.colours angle, level_mv, swing_mv, r_y and b_y must be finite"
            ));
        }
        if list.iter().any(|c| {
            c.r_y.is_some() != c.b_y.is_some() || (c.r_y.is_some() && (c.angle.is_some() || c.swing_mv.is_some()))
        }) {
            return Err(format!(
                "{file}: signal.colours r_y and b_y go together and replace angle and swing_mv"
            ));
        }
        if list.iter().any(|c| c.swing_mv.is_some()) && self.chroma_factor.is_none() {
            return Err(format!(
                "{file}: signal.chroma_factor is needed by the colours with a swing_mv"
            ));
        }
        if self.revisions.is_empty() || self.revisions.len() > MAX_PALETTES {
            return Err(format!(
                "{file}: signal.revisions must hold 1 to {MAX_PALETTES} revisions"
            ));
        }
        // The reference palette, once per exponent: the set `k` fills the entries `k x n` to `k x n + n - 1`.
        let mut references = [[0.0; 3]; SIGNAL_MAX];
        if let Some(refs) = rgb_refs {
            for (i, x) in refs.iter().enumerate() {
                references[i] = [(x.rgb >> 16) & 255, (x.rgb >> 8) & 255, x.rgb & 255].map(|v| f64::from(v) / 255.0);
            }
        }
        let computed_sets = if rgb_refs.is_some() { 0 } else { sets };
        for (k, gamma) in std::iter::once(r.gamma)
            .chain(extra_gammas.iter().copied())
            .take(computed_sets)
            .enumerate()
        {
            let set = self.colours_for(
                file,
                "signal.reference",
                &list,
                &SignalSettings {
                    luma_mv: ref_luma.as_deref(),
                    r_y: None,
                    b_y: None,
                    neutral: None,
                    black_mv: r.black_mv,
                    white_mv: r.white_mv,
                    phase_row_deg: r.phase_row_deg.unwrap_or(0.0),
                    amplitude: r.amplitude,
                    saturation: r.saturation.unwrap_or(1.0),
                    gain: r.gain,
                    gamma,
                },
            )?;
            references[k * n..(k + 1) * n].copy_from_slice(&set[..n]);
        }
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
            if let Some(a) = rev.amplitude {
                positive(file, "signal.revisions", "amplitude", a)?;
            }
            let rev_luma = match &rev.luma_mv {
                None => None,
                Some(l) => Some(
                    self.levels(l, n)
                        .filter(|l| l.iter().all(|v| v.is_finite() && *v >= 0.0))
                        .ok_or_else(|| {
                            format!(
                                "{file}: signal.revisions {} luma_mv must hold {n} levels, not negative (a grid: one per luma)",
                                rev.id
                            )
                        })?,
                ),
            };
            if rev.r_y.is_some() != rev.b_y.is_some() {
                return Err(format!("{file}: signal.revisions {} r_y and b_y go together", rev.id));
            }
            let rev_differences = match (&rev.r_y, &rev.b_y) {
                (Some(ry), Some(by)) => Some(
                    (ry.len() == n && by.len() == n && ry.iter().chain(by).all(|v| v.is_finite()))
                        .then_some((ry.as_slice(), by.as_slice()))
                        .ok_or_else(|| {
                            format!(
                                "{file}: signal.revisions {} r_y and b_y must hold {n} finite values each",
                                rev.id
                            )
                        })?,
                ),
                _ => None,
            };
            if rev.neutral.is_some_and(|v| !v.is_finite()) {
                return Err(format!("{file}: signal.revisions {} neutral must be finite", rev.id));
            }
            let colours = self.colours_for(
                file,
                &format!("signal.revisions {}", rev.id),
                &list,
                &SignalSettings {
                    luma_mv: rev_luma.as_deref(),
                    r_y: rev_differences.map(|d| d.0),
                    b_y: rev_differences.map(|d| d.1),
                    neutral: rev.neutral,
                    black_mv: rev.black_mv,
                    white_mv: rev.white_mv,
                    phase_row_deg: rev.phase_row_deg.unwrap_or(0.0),
                    amplitude: rev.amplitude,
                    saturation: 1.0,
                    gain: 1.0,
                    gamma: 1.0,
                },
            )?;
            // One copy of the colours per reference set: the entry `k x n + i` of a later set gives the colour `i`.
            let mut table = [[0.0; 3]; SIGNAL_MAX];
            let count = match rgb_refs {
                // One entry per reference colour, the colour that reference stands for.
                Some(refs) => {
                    for (i, x) in refs.iter().enumerate() {
                        table[i] = colours[x.colour];
                    }
                    refs.len()
                }
                None => {
                    for k in 0..sets {
                        table[k * n..(k + 1) * n].copy_from_slice(&colours[..n]);
                    }
                    n * sets
                }
            };
            out.push(Variant {
                id: rev.id.clone(),
                name: rev.name.clone(),
                transform: Transform::Signal(Signal {
                    colours: table,
                    count,
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
            scale: 1.0,
        })
    }

    /// The keys that are present, for the provenance check.
    fn keys(&self) -> Vec<&'static str> {
        let mut k = vec!["table", "input_bits", "output_max"];
        if self.variants.is_some() {
            k.push("variants");
        }
        k
    }

    /// The variants of the curve: the base curve with each variant's scale, the first one the target's own.
    fn curve_variants(&self, file: &str, base: &Curve) -> Result<Vec<Variant>, String> {
        let Some(list) = &self.variants else {
            return Ok(Vec::new());
        };
        if list.is_empty() || list.len() > MAX_PALETTES {
            return Err(format!("{file}: curve.variants must hold 1 to {MAX_PALETTES} variants"));
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
                    "{file}: curve.variants id \"{}\" must be lower case letters, digits and '_'",
                    v.id
                ));
            }
            if out.iter().any(|x| x.id == v.id) {
                return Err(format!("{file}: curve.variants id \"{}\" is used twice", v.id));
            }
            positive(file, "curve.variants", "scale", v.scale)?;
            if v.scale > 1.0 {
                return Err(format!("{file}: curve.variants {} scale must be at most 1", v.id));
            }
            out.push(Variant {
                id: v.id.clone(),
                name: v.name.clone(),
                transform: Transform::Curve(Curve {
                    scale: v.scale,
                    ..*base
                }),
            });
        }
        Ok(out)
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
        let variants: Vec<Variant>;
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
            (_, Some(c), ..) => {
                let base = c.curve(file)?;
                variants = c.curve_variants(file, &base)?;
                let first = variants.first().map_or(Transform::Curve(base), |v| v.transform);
                (first, c.keys())
            }
            (_, _, Some(p), ..) => {
                variants = p.variants(file)?;
                (variants[0].transform, vec!["sets", "colour_bits"])
            }
            (_, _, _, Some(d), ..) => {
                variants = d.variants(file)?;
                (variants[0].transform, d.keys())
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

    /// The Atari 8-bit file: 256 colours indexed by the colour register (hue x 16 + luma), six variants, the first the
    /// 800XL at the middle hue delay; every provenance key present; hand-derived colours. 130XE, hue 5, luma 12 (`$5C`):
    /// Y = (1196 - 468) / (1380 - 468) = 0.79825, the phase 180 - 4 x 25.5 = 78 degrees, the amplitude 0.233: U = 0.04844, V =
    /// 0.22791; R = Y + V / 0.877283 = 1.0580 (clamped to 1), B = Y + U / 0.492111 = 0.89668, G = (Y - 0.299 R - 0.114 B) /
    /// 0.587 = 0.6468 (from the unclamped red, so the luma is kept). 800XL hue 1 luma 8 (`$18`): Y = 0.51799, the burst phase,
    /// U = -0.239, V = 0: R = 0.51799, B = 0.03233, G = 0.6123.
    #[test]
    fn the_atari_file_and_its_hand_derived_colours() {
        let t = for_value(crate::params::SOURCE_COLOUR_TARGET_ATARI_8BIT_GTIA).expect("atari");
        let ids: Vec<&str> = t.variants.iter().map(|v| v.id.as_str()).collect();
        assert_eq!(
            ids,
            ["800xl", "130xe", "800", "400", "800xl_delay_23_5", "800xl_delay_27_5"]
        );
        for key in [
            "grid",
            "revisions",
            "reference",
            "threshold",
            "black_index",
            "white_index",
        ] {
            assert!(t.provenance.contains_key(key), "provenance.{key}");
        }
        assert!(!t.provenance.contains_key("colours"));
        assert_eq!(t.select(1), t.transform);
        let Transform::Signal(s) = t.transform else {
            panic!("signal")
        };
        assert_eq!(s.count, 256);
        assert_eq!(s.colours[0x00], [0.0; 3], "$00 is black");
        assert_eq!(s.colours[0x0F], [1.0; 3], "$0F is white");
        let [r, g, b] = s.colours[0x18];
        assert!(
            (r - 0.51799).abs() < 5e-4 && (g - 0.6123).abs() < 5e-4 && (b - 0.03233).abs() < 5e-4,
            "{r} {g} {b}"
        );
        // Hue 0 is the 16 greys of the machine's luma: $08 on the 800XL is (1688 - 1112) / 1112.
        for l in 0..16 {
            let c = s.colours[l];
            assert!(c.iter().all(|v| (v - c[0]).abs() < 1e-12), "grey {l}: {c:?}");
        }
        assert!((s.colours[0x08][0] - (1688.0 - 1112.0) / 1112.0).abs() < 1e-12);
        // The greys rise, with brightness 7 and 8 closer than the others (the 4.7k ladder resistor).
        assert!((0..15).all(|l| s.colours[l][0] < s.colours[l + 1][0]));
        let step = |l: usize| s.colours[l + 1][0] - s.colours[l][0];
        assert!(
            step(7) < 0.6 * step(6) && step(7) < 0.6 * step(9),
            "{} {} {}",
            step(6),
            step(7),
            step(9)
        );
        let Transform::Signal(xe) = t.transform_for(1) else {
            panic!("signal")
        };
        let [r, g, b] = xe.colours[0x5C];
        assert!(
            r == 1.0 && (g - 0.6468).abs() < 1e-3 && (b - 0.89668).abs() < 1e-3,
            "{r} {g} {b}"
        );
        // The 800 is the most saturated machine: its hue 10 luma 8 (no channel clamps there) sits 0.35 / 0.239 farther from the grey of its luma.
        let sat = |s: &Signal, i: usize| dist(s.colours[i], s.colours[i & 15]);
        let Transform::Signal(a800) = t.transform_for(2) else {
            panic!("signal")
        };
        assert!(
            sat(&a800, 0xA8) > sat(&s, 0xA8) * 1.3,
            "{} {}",
            sat(&a800, 0xA8),
            sat(&s, 0xA8)
        );
        // Hue 1 is the burst phase on every delay; the other hues move with the delay: $18 is the same colour, $F8 is not.
        let (lo, hi) = (t.transform_for(4), t.transform_for(5));
        let (Transform::Signal(lo), Transform::Signal(hi)) = (lo, hi) else {
            panic!("signal")
        };
        assert_eq!(lo.colours[0x18], s.colours[0x18]);
        assert_eq!(hi.colours[0x18], s.colours[0x18]);
        assert_ne!(lo.colours[0xF8], hi.colours[0xF8]);
        // The classification does not depend on the machine: the references are the same for every variant.
        for v in 1..6 {
            let Transform::Signal(o) = t.transform_for(v) else {
                panic!("signal")
            };
            assert_eq!(o.references, s.references, "variant {v}");
        }
    }

    /// Recognition: every colour of the reference palette is its own nearest (the two closest are printed), the output of a
    /// recognised pixel is the machine's colour, and the computed palettes of the machines (another saturation, another
    /// luma, another hue delay) land on a reference colour within 0.1 of the intended colour for 248 to 256 of the 256
    /// colours, the saturated 800 for most; a pixel far from every colour passes through.
    #[test]
    fn the_atari_reference_classifies_itself_and_the_computed_palettes_of_the_machines() {
        let t = for_value(crate::params::SOURCE_COLOUR_TARGET_ATARI_8BIT_GTIA).expect("atari");
        let Transform::Signal(s) = t.transform else {
            panic!("signal")
        };
        let mut closest = (f64::MAX, 0, 0);
        for i in 0..256 {
            assert_eq!(s.classify(s.reference(i)), Some(i), "colour {i:#04X}");
            for j in i + 1..256 {
                let d = dist(s.reference(i), s.reference(j));
                if d < closest.0 {
                    closest = (d, i, j);
                }
            }
        }
        println!(
            "closest two reference colours: {:#04X} and {:#04X}, {:.4}; threshold {}",
            closest.1, closest.2, closest.0, s.threshold
        );
        assert!(closest.0 > 0.001);
        // A recognised pixel is replaced by this machine's colour; hue 1 luma 8 of the reference gives the 800XL's $18.
        assert_eq!(s.apply(s.reference(0x18)), s.colours[0x18]);
        for (v, name, need) in [
            (0, "800XL", 256),
            (1, "130XE", 256),
            (2, "800", 160),
            (3, "400", 256),
            (4, "800XL 23.5", 248),
            (5, "800XL 27.5", 248),
        ] {
            let Transform::Signal(o) = t.transform_for(v) else {
                panic!("signal")
            };
            let (mut exact, mut near, mut worst) = (0, 0, 0.0f64);
            for i in 0..256 {
                let got = s.classify(o.colours[i]);
                exact += usize::from(got == Some(i));
                if let Some(j) = got {
                    let d = dist(s.reference(j), o.colours[i]);
                    worst = worst.max(d);
                    near += usize::from(d < 0.1);
                }
            }
            println!("{name}: {exact} of 256 exact, {near} within 0.1, worst {worst:.3}");
            assert!(near >= need, "{name}: {near} within 0.1");
        }
        // A saturated green is on no hue-and-luma curve of the reference at the threshold: it passes through.
        let odd = [0.0, 1.0, 0.0];
        assert_eq!(s.classify(odd), None, "{:?}", s.classify(odd));
        assert_eq!(s.apply(odd), odd);
    }

    /// The grid validation: too many colours, a non finite angle, a list of colours next to the grid, a luma list of the
    /// wrong length, a missing provenance, a zero amplitude, a black index past the colours.
    #[test]
    fn a_bad_grid_file_is_refused() {
        let ok = SHIPPED
            .iter()
            .find(|(f, _)| *f == "atari_8bit_gtia.toml")
            .expect("the atari file")
            .1
            .to_string();
        assert!(Target::parse("t.toml", &ok).is_ok());
        let cases = [
            (ok.replace("hues = 16", "hues = 17"), "grid"),
            (ok.replace("angle = 180.0", "angle = inf"), "grid"),
            (format!("{ok}\n[[signal.colours]]\nname = \"x\"\n"), "alternatives"),
            (ok.replacen("luma_mv = [1112, 1184,", "luma_mv = [1112,", 1), "luma_mv"),
            (ok.replace("grid = \"K:", "grid = \"X:"), "provenance.grid"),
            (ok.replacen("amplitude = 0.239", "amplitude = 0", 1), "amplitude"),
            (ok.replace("black_index = 0", "black_index = 400"), "black"),
        ];
        for (text, key) in cases {
            let err = Target::parse("t.toml", &text).expect_err(key);
            assert!(err.contains("t.toml") && err.contains(key), "{key}: {err}");
        }
    }

    /// Y, R-Y and B-Y in percent of the 15 colours of the TMS9928A and TMS9929A, colour codes 1 to 15 (Table 2-3 of the
    /// Texas Instruments data manual of November 1982, as the Wikipedia article 'TMS9918' reproduces it), typed here
    /// independently of the data file.
    const TMS: [(&str, f64, f64, f64); 15] = [
        ("black", 0.0, 47.0, 47.0),
        ("medium green", 53.0, 7.0, 20.0),
        ("light green", 67.0, 17.0, 27.0),
        ("dark blue", 40.0, 40.0, 100.0),
        ("light blue", 53.0, 43.0, 93.0),
        ("dark red", 47.0, 83.0, 30.0),
        ("cyan", 73.0, 0.0, 70.0),
        ("medium red", 53.0, 93.0, 27.0),
        ("light red", 67.0, 93.0, 27.0),
        ("dark yellow", 73.0, 57.0, 7.0),
        ("light yellow", 80.0, 57.0, 17.0),
        ("dark green", 47.0, 13.0, 23.0),
        ("magenta", 53.0, 73.0, 67.0),
        ("grey", 80.0, 47.0, 47.0),
        ("white", 100.0, 47.0, 47.0),
    ];

    /// The channels of colour `i` of [`TMS`] before the rails clamp them, decoded as `scale_r` and `scale_b` times the
    /// colour differences (1 and 1 is the decode of the file); G' comes from the BT.601 luma equation, or (with
    /// `g_from_differences`) from the BT.601 Pb and Pr matrix, G' = Y - 0.714 Pr - 0.344 Pb.
    fn tms_decode(i: usize, scale_r: f64, scale_b: f64, g_from_differences: bool) -> [f64; 3] {
        let (_, y, ry, by) = TMS[i];
        let (y, ry, by) = (y / 100.0, (ry - 47.0) / 100.0, (by - 47.0) / 100.0);
        let (r, b) = (y + scale_r * ry, y + scale_b * by);
        let g = if g_from_differences {
            y - 0.714 * ry - 0.344 * by
        } else {
            (y - 0.299 * r - 0.114 * b) / 0.587
        };
        [r, g, b]
    }

    /// The TMS9928A file: 15 colours in two reference sets (30 entries), two variants; the colours are the decode of the
    /// data manual's levels written out here, hand-derived entries, and the scope readings agree with the data manual.
    /// Medium green (Y 0.53, R-Y 0.07 - 0.47 = -0.40, B-Y 0.20 - 0.47 = -0.27): R' = 0.13, B' = 0.26, G' = (0.53 - 0.299 x 0.13
    /// minus 0.114 x 0.26) / 0.587 = 0.78622. Light red (Y 0.67, R-Y 0.46, B-Y -0.20): R' = 1.13 (clamped to 1), B' = 0.47,
    /// and G' = (0.67 - 0.299 x 1.13 - 0.114 x 0.47) / 0.587 = 0.47453, from the unclamped red.
    #[test]
    fn the_tms9928_file_and_its_decode() {
        let t = for_value(crate::params::SOURCE_COLOUR_TARGET_TEXAS_INSTRUMENTS_TMS9928).expect("tms9928");
        let ids: Vec<&str> = t.variants.iter().map(|v| v.id.as_str()).collect();
        assert_eq!(ids, ["datasheet", "sc3000_scope"]);
        for key in [
            "colours",
            "neutral",
            "revisions",
            "reference",
            "threshold",
            "black_index",
            "white_index",
        ] {
            assert!(t.provenance.contains_key(key), "provenance.{key}");
        }
        assert_eq!(t.select(1), t.transform);
        let Transform::Signal(s) = t.transform else {
            panic!("signal")
        };
        // 15 colours in two reference sets: the second set repeats the colours.
        assert_eq!(s.count, 30);
        assert_eq!(s.colours[..15], s.colours[15..30]);
        for (i, (name, ..)) in TMS.iter().enumerate() {
            let want = tms_decode(i, 1.0, 1.0, false).map(|c| c.clamp(0.0, 1.0));
            assert!(dist(s.colours[i], want) < 1e-9, "{name}: {:?} {want:?}", s.colours[i]);
        }
        let [r, g, b] = s.colours[1];
        assert!(
            (r - 0.13).abs() < 1e-9 && (g - 0.78622).abs() < 1e-4 && (b - 0.26).abs() < 1e-9,
            "{r} {g} {b}"
        );
        let [r, g, b] = s.colours[8];
        assert!(
            r == 1.0 && (g - 0.47453).abs() < 1e-4 && (b - 0.47).abs() < 1e-9,
            "{r} {g} {b}"
        );
        assert_eq!(s.colours[0], [0.0; 3], "black");
        assert_eq!(s.colours[14], [1.0; 3], "white");
        assert!(
            s.colours[13].iter().all(|c| (c - 0.8).abs() < 1e-12),
            "grey is 80 percent"
        );
        // The two readings of the chip agree: the scope variant, divided by its white of 964 mV, is within 0.025 of the
        // data manual's colours in every channel (the largest differences are in the magenta and the dark blue).
        let Transform::Signal(scope) = t.transform_for(1) else {
            panic!("signal")
        };
        let mut worst = (0.0f64, 0);
        for i in 0..15 {
            for c in 0..3 {
                let d = (scope.colours[i][c] - s.colours[i][c]).abs();
                if d > worst.0 {
                    worst = (d, i);
                }
            }
        }
        println!(
            "scope against the data manual: worst channel difference {:.4} ({})",
            worst.0, TMS[worst.1].0
        );
        assert!(worst.0 < 0.025, "{worst:?}");
        // Dark yellow is a yellow, from the scope as well: red above green above blue, green at 0.9 of the red or more.
        for v in [&s, &scope] {
            let [r, g, b] = v.colours[9];
            assert!(r > g && g > b && g > 0.88 * r && b < 0.4, "{r} {g} {b}");
        }
        // Light blue on the scope reading is past the rail (B' 1.012) and clamps.
        assert_eq!(scope.colours[4][2], 1.0);
        // The decode that was ruled out: the differences as BT.601 Pb and Pr (1.402 and 1.772 times) put five channels above 1.1,
        // the decode kept only one (the red of light red); no colour but black has a channel below 0.12 in the decode kept.
        let above = |kr: f64, kb: f64, diff_g: bool| {
            (0..15)
                .flat_map(|i| tms_decode(i, kr, kb, diff_g))
                .filter(|c| *c > 1.1)
                .count()
        };
        assert_eq!(above(1.0, 1.0, false), 1);
        assert!(above(1.402, 1.772, true) >= 5, "{}", above(1.402, 1.772, true));
        assert!((1..15).all(|i| tms_decode(i, 1.0, 1.0, false).iter().all(|c| *c > 0.12)));
        // The forum's scaling (B-Y x 0.564, R-Y x 0.713, then BT.601: x 1.402 and x 1.772) is the unit scale to 0.1 percent.
        assert!((1.402_f64 * 0.713 - 1.0).abs() < 1e-3 && (1.772_f64 * 0.564 - 1.0).abs() < 1e-3);
        // The luma weights of 1953 NTSC and BT.601 differ by at most 0.005 in any channel (the green of dark blue, 0.0040).
        for (i, (name, y, ..)) in TMS.iter().enumerate() {
            let [r, g, b] = tms_decode(i, 1.0, 1.0, false);
            let g_1953 = (y / 100.0 - 0.30 * r - 0.11 * b) / 0.59;
            assert!((g_1953 - g).abs() < 0.005, "{name}");
        }
    }

    /// The Wikipedia 'TMS9918' article's palette (CC BY-SA): the decode of the data manual raised to 1.6, in hex, as an
    /// emulator shows the colours; the 'colour code 1 to 15' order, black first.
    const TMS_WIKIPEDIA: [&str; 15] = [
        "000000", "0AAD1E", "34C84C", "2B2DE3", "514BFB", "BD2925", "1EE2EF", "FB2C2B", "FF5F4C", "BDA22B", "D7B454",
        "0A8C18", "AF329A", "B2B2B2", "FFFFFF",
    ];

    /// Recognition: every reference entry is its own nearest (up to the entry of the same colour in the other set), the
    /// Wikipedia palette is recognised in full, the decode raised to any exponent from 1.0 to 1.6 loses at most one colour
    /// of 15 to a neighbour, the closest two colours are printed, and a pixel far from every colour passes through.
    #[test]
    fn the_tms9928_reference_recognises_the_published_palette_and_the_gamma_family() {
        let t = for_value(crate::params::SOURCE_COLOUR_TARGET_TEXAS_INSTRUMENTS_TMS9928).expect("tms9928");
        let Transform::Signal(s) = t.transform else {
            panic!("signal")
        };
        let mut closest = (f64::MAX, 0, 0);
        for i in 0..30 {
            let got = s.classify(s.reference(i)).expect("a reference colour is recognised");
            assert_eq!(got % 15, i % 15, "entry {i}");
            for j in 0..30 {
                if i % 15 != j % 15 {
                    let d = dist(s.reference(i), s.reference(j));
                    if d < closest.0 {
                        closest = (d, i, j);
                    }
                }
            }
        }
        println!(
            "closest two reference entries of different colours: {} and {}, {:.4}; threshold {}",
            TMS[closest.1 % 15].0,
            TMS[closest.2 % 15].0,
            closest.0,
            s.threshold
        );
        assert!(closest.0 > 0.03, "{closest:?}");
        // A recognised pixel is replaced by the chip's colour, whichever set it matched.
        assert_eq!(s.apply(s.reference(2)), s.colours[2]);
        assert_eq!(s.apply(s.reference(17)), s.colours[2]);
        // The published palette: all 15 colours recognised as their own, within 0.07.
        let mut worst = 0.0f64;
        for (i, h) in TMS_WIKIPEDIA.iter().enumerate() {
            let got = s
                .classify(hex(h))
                .unwrap_or_else(|| panic!("{} not recognised", TMS[i].0));
            assert_eq!(got % 15, i, "{}", TMS[i].0);
            worst = worst.max(dist(s.reference(got), hex(h)));
        }
        println!("Wikipedia palette: all 15 recognised, farthest {worst:.4} from its reference colour");
        assert!(worst < 0.07, "{worst}");
        // The decode at other display gammas (what an emulator may apply): at most one colour of 15 is mistaken.
        for step in 0..=24 {
            let gamma = 1.0 + 0.025 * f64::from(step);
            let wrong = (0..15)
                .filter(|&i| {
                    let c = tms_decode(i, 1.0, 1.0, false).map(|v| v.clamp(0.0, 1.0).powf(gamma));
                    s.classify(c).map(|j| j % 15) != Some(i)
                })
                .count();
            assert!(wrong <= 1, "gamma {gamma}: {wrong} colours mistaken");
        }
        // Another colour, far from all 15, passes through.
        let odd = [0.0, 1.0, 0.0];
        assert_eq!(s.classify(odd), None);
        assert_eq!(s.apply(odd), odd);
        // The classification does not depend on the variant.
        let Transform::Signal(scope) = t.transform_for(1) else {
            panic!("signal")
        };
        assert_eq!(scope.references, s.references);
    }

    /// The colour-difference and reference-set validation: one difference without the other, a difference next to an angle,
    /// a revision list of the wrong length, a zero exponent, more colours than the table holds, a neutral that is not finite.
    #[test]
    fn a_bad_tms9928_file_is_refused() {
        let ok = SHIPPED
            .iter()
            .find(|(f, _)| *f == "texas_instruments_tms9928.toml")
            .expect("the tms file")
            .1
            .to_string();
        assert!(Target::parse("t.toml", &ok).is_ok());
        let sets: Vec<String> = (0..17).map(|k| format!("{}", 1.1 + 0.01 * f64::from(k))).collect();
        let cases = [
            (ok.replacen("b_y = 200\n", "", 1), "r_y and b_y"),
            (
                ok.replacen(
                    "name = \"medium green\"\n",
                    "name = \"medium green\"\nangle = 10.0\n",
                    1,
                ),
                "replace angle",
            ),
            (ok.replacen("r_y = [0, -384,", "r_y = [-384,", 1), "must hold 15"),
            (
                ok.replace("extra_gammas = [1.5]", "extra_gammas = [0.0]"),
                "extra_gammas",
            ),
            (
                ok.replace("extra_gammas = [1.5]", &format!("extra_gammas = [{}]", sets.join(", "))),
                "reference sets",
            ),
            (ok.replacen("neutral = 470.0", "neutral = inf", 1), "neutral"),
            (ok.replace("neutral = \"S:", "neutral = \"X:"), "provenance.neutral"),
        ];
        for (text, key) in cases {
            let err = Target::parse("t.toml", &text).expect_err(key);
            assert!(err.contains("t.toml") && err.contains(key), "{key}: {err}");
        }
    }

    /// The Neo Geo file: the 32 levels are the resistor ladder of 3900, 2200, 1000, 470 and 220 ohm rounded to whole 8-bit
    /// codes (recomputed here from the resistors), they rise strictly from 0 to the reference white 255, and the four
    /// variants carry the factors of the dark bit (8200 ohm), the shadow (150 ohm) and both, recomputed from the
    /// resistors; the first variant is the target's own and has no factor.
    #[test]
    fn the_neo_geo_file_holds_the_ladder_levels_and_the_pulldown_factors() {
        use crt_model::target::ladder_output;
        const R: [f64; 5] = [3900.0, 2200.0, 1000.0, 470.0, 220.0];
        let t = for_value(crate::params::SOURCE_COLOUR_TARGET_SNK_NEO_GEO).expect("neo geo");
        let Transform::Curve(c) = t.transform else {
            panic!("curve")
        };
        assert_eq!((c.table[0], c.table[31], c.output_max, c.scale), (0, 255, 255.0, 1.0));
        assert!(c.table.windows(2).all(|w| w[0] < w[1]), "the table rises strictly");
        for (code, entry) in c.table.iter().enumerate() {
            let exact = 255.0 * ladder_output(&R, code as u32, None);
            assert!(
                (exact - f64::from(*entry)).abs() <= 0.5,
                "code {code}: ladder {exact}, table {entry}"
            );
        }
        // The levels a plain 5-bit picture shows, 8 x 8 + 2 = 66 for code 8, 132 for code 16: 65 and 138.
        assert_eq!((c.table[4], c.table[8], c.table[16]), (30, 65, 138));
        let ids: Vec<&str> = t.variants.iter().map(|v| v.id.as_str()).collect();
        assert_eq!(ids, ["normal", "dark", "shadow", "shadow_dark"]);
        let pulls = [
            None,
            Some(8200.0),
            Some(150.0),
            Some(1.0 / (1.0 / 8200.0 + 1.0 / 150.0)),
        ];
        for (v, pull) in t.variants.iter().zip(pulls) {
            let Transform::Curve(vc) = v.transform else {
                panic!("curve")
            };
            assert_eq!(vc.table, c.table, "{}: the same levels", v.id);
            // The factor is the same for every code: the ratio with and without the pulldown.
            for code in 1..32 {
                let want = ladder_output(&R, code, pull) / ladder_output(&R, code, None);
                assert!(
                    (vc.scale - want).abs() < 2e-6,
                    "{} code {code}: scale {} want {want}",
                    v.id,
                    vc.scale
                );
            }
        }
        assert_eq!(t.select(1), t.transform);
        assert_eq!(t.select(0), t.transform, "below 1: the first");
        assert_eq!(t.select(9), t.transform, "past the last: the first");
        assert_ne!(t.select(2), t.select(3));
        // Through the oracle: grey 16 of a plain picture (132 of 255) is 138 of 255; the shadow lowers white to 0.557054.
        let grey = f64::from(crt_model::target::expand(16, 5)) / 255.0;
        assert!((t.select(1).apply([grey; 3])[0] - 138.0 / 255.0).abs() < 1e-12);
        assert!((t.select(3).apply([1.0; 3])[0] - 0.557_054).abs() < 1e-9);
        assert!((t.select(2).apply([1.0; 3])[1] - 0.985_663).abs() < 1e-9);
        assert_eq!(t.select(3).apply([0.0; 3]), [0.0; 3]);
        assert_eq!(t.provenance["variants"].chars().next(), Some('D'));
    }

    /// The curve variant validation: a scale of 0 or above 1, a repeated or empty id, an empty list, an unknown key, a
    /// missing provenance entry for the variants; a curve without variants still parses (the SNES file) and has none.
    #[test]
    fn a_bad_curve_variant_is_refused() {
        let ok = SHIPPED
            .iter()
            .find(|(f, _)| *f == "snk_neo_geo.toml")
            .expect("the neo geo file")
            .1
            .to_string();
        assert!(Target::parse("t.toml", &ok).is_ok());
        let cases = [
            (ok.replacen("scale = 1.0", "scale = 0.0", 1), "scale"),
            (ok.replacen("scale = 1.0", "scale = 1.5", 1), "at most 1"),
            (ok.replace("id = \"dark\"", "id = \"normal\""), "twice"),
            (ok.replacen("id = \"dark\"", "id = \"Dark\"", 1), "lower case"),
            (ok.replacen("scale = 1.0", "scale = 1.0\nextra = 1", 1), "extra"),
            (ok.replace("variants = \"D:", "variants = \"X:"), "provenance.variants"),
            (ok.replace("input_bits = 5", "input_bits = 4"), "input_bits"),
        ];
        for (text, key) in cases {
            let err = Target::parse("t.toml", &text).expect_err(key);
            assert!(err.contains("t.toml") && err.contains(key), "{key}: {err}");
        }
        let snes = for_value(crate::params::SOURCE_COLOUR_TARGET_SUPER_NINTENDO_BLACK_BOOST).expect("snes");
        assert!(snes.variants.is_empty());
        assert_eq!(snes.select(3), snes.transform, "no variants: the target's own");
    }

    /// The Master System file: three variants on a plain 2-bit picture (0, 85, 170, 255), 4 codes padded to 8 with the last
    /// point; the outputs are TmEE's millivolts scaled to the top level (SMS1 0, 324, 620, 916; SMS2 0, 364, 720, 1052,
    /// the second step within 1 of the scaling, the post's 0.350 giving 89 where the millivolts give 88.2), the first VDP's
    /// blue first step is the scope's 22 to 23 percent over 85 (104) or over TmEE's 90 (110), the second VDP has none.
    #[test]
    fn the_master_system_file_holds_the_measured_levels_and_the_blue_of_the_first_vdp() {
        let t = for_value(crate::params::SOURCE_COLOUR_TARGET_SEGA_MASTER_SYSTEM).expect("master system");
        let ids: Vec<&str> = t.variants.iter().map(|v| v.id.as_str()).collect();
        assert_eq!(ids, ["sms1_bench", "sms1_scope", "sms2_bench"]);
        let dacs: Vec<Dac> = t
            .variants
            .iter()
            .map(|v| match v.transform {
                Transform::Dac(d) => d,
                _ => panic!("dac"),
            })
            .collect();
        for d in &dacs {
            // Four codes, the last point repeated; the input is the plain 2-bit expansion.
            assert_eq!(d.input, [0, 85, 170, 255, 255, 255, 255, 255]);
            assert!(d.output[3..].iter().all(|&o| o == 255));
            assert_eq!(d.output_max, 255.0);
            assert_eq!(d.output_green, None, "green is red's on every variant");
            for i in 0..4 {
                assert_eq!(d.input[i], 85 * i as u8, "c x 85");
            }
        }
        assert_eq!(
            t.transform, t.variants[0].transform,
            "the first variant is the target's own"
        );
        // The levels: TmEE's millivolts scaled to the top level.
        let scaled = |mv: [f64; 4]| mv.map(|m| m / mv[3] * 255.0);
        for (got, want) in dacs[0].output.iter().zip(scaled([0.0, 324.0, 620.0, 916.0])) {
            assert!((want - f64::from(*got)).abs() <= 0.5, "SMS1 {want} vs {got}");
        }
        for (got, want) in dacs[2].output.iter().zip(scaled([0.0, 364.0, 720.0, 1052.0])) {
            assert!((want - f64::from(*got)).abs() <= 1.0, "SMS2 {want} vs {got}");
        }
        assert_eq!(dacs[0].output[..4], [0, 90, 173, 255]);
        assert_eq!(dacs[2].output[..4], [0, 89, 174, 255]);
        assert_eq!(
            dacs[1].output[..4],
            [0, 85, 170, 255],
            "the scope: red and green linear"
        );
        // The blue: only the first step is raised, by 22 to 23 percent over the reference of its variant (the scope's
        // linear 85, TmEE's 90); the second VDP has no blue of its own.
        let b1 = dacs[1].output_blue.expect("blue")[1];
        let b0 = dacs[0].output_blue.expect("blue")[1];
        assert!(
            (f64::from(b1) / 85.0 - 1.0).abs() > 0.22 && (f64::from(b1) / 85.0 - 1.0) < 0.23,
            "{b1}"
        );
        assert!(
            (f64::from(b0) / 90.0 - 1.0) > 0.21 && (f64::from(b0) / 90.0 - 1.0) < 0.24,
            "{b0}"
        );
        assert_eq!((b1, b0), (104, 110));
        assert_eq!(
            dacs[0].output_blue.expect("blue")[2..4],
            dacs[0].output[2..4],
            "only the first step of blue"
        );
        assert_eq!(dacs[1].output_blue.expect("blue")[2..4], dacs[1].output[2..4]);
        assert_eq!(dacs[2].output_blue, None);
        // Through the oracle: the 2-bit code 1 (85) of a picture is 90 on red and 110 on blue on the first VDP, 104 on
        // blue on the scope reading, and 89 on all three on the second.
        let px = [85.0 / 255.0; 3];
        let first = t.select(1).apply(px);
        assert!((first[0] - 90.0 / 255.0).abs() < 1e-12 && (first[1] - 90.0 / 255.0).abs() < 1e-12);
        assert!((first[2] - 110.0 / 255.0).abs() < 1e-12);
        assert!((t.select(2).apply(px)[2] - 104.0 / 255.0).abs() < 1e-12);
        let second = t.select(3).apply(px);
        assert!(second.iter().all(|c| (c - 89.0 / 255.0).abs() < 1e-12), "{second:?}");
        // The ends and the code 3 (255): white stays full on every variant, black stays black.
        for v in 1..=3 {
            assert_eq!(t.select(v).apply([1.0; 3]), [1.0; 3]);
            assert_eq!(t.select(v).apply([0.0; 3]), [0.0; 3]);
        }
        assert_eq!(t.select(0), t.transform);
        assert_eq!(t.select(9), t.transform, "past the last: the first");
        assert_eq!(t.provenance["variant_outputs"].chars().next(), Some('E'));
    }

    /// The dac validation of the Master System's own fields: a variant output of the wrong length, a green or blue with no
    /// output of its own, a decreasing blue, an input that is not strictly increasing in its four codes, a missing
    /// provenance entry for the variants' outputs; a file whose variants have none of their own needs no such entry.
    #[test]
    fn a_bad_master_system_file_is_refused() {
        let ok = SHIPPED
            .iter()
            .find(|(f, _)| *f == "sega_master_system.toml")
            .expect("the master system file")
            .1
            .to_string();
        assert!(Target::parse("t.toml", &ok).is_ok());
        let cases = [
            (
                ok.replacen("output_blue = [0, 110, 173, 255]", "output_blue = [0, 110, 173]", 1),
                "must hold 4",
            ),
            (
                ok.replacen(
                    "output_blue = [0, 110, 173, 255]",
                    "output_blue = [0, 200, 173, 255]",
                    1,
                ),
                "must not decrease",
            ),
            (
                ok.replacen("output = [0, 85, 170, 255]\n", "", 1),
                "need its own output",
            ),
            (
                ok.replacen("levels = [0, 85, 170, 255]", "levels = [0, 85, 85, 255]", 1),
                "strictly increase",
            ),
            (
                ok.replacen("levels = [0, 85, 170, 255]", "levels = [0, 85, 170, 256]", 1),
                "0 to 255",
            ),
            (
                ok.replacen("levels = [0, 85, 170, 255]", "levels = [0, 85, 170, 255, 255]", 1),
                "must hold 4",
            ),
            (
                ok.replace("output = [0, 90, 173, 255]\noutput_max", "output = [0]\noutput_max"),
                "2 to 8",
            ),
            (
                ok.replace("variant_outputs = \"E:", "variant_outputs = \"X:"),
                "provenance.variant_outputs",
            ),
        ];
        for (text, key) in cases {
            let err = Target::parse("t.toml", &text).expect_err(key);
            assert!(err.contains("t.toml") && err.contains(key), "{key}: {err}");
        }
        // The Mega Drive file has no variant outputs of its own and no such provenance entry.
        let md = for_value(SOURCE_COLOUR_TARGET_SEGA_MEGA_DRIVE).expect("md");
        assert!(!md.provenance.contains_key("variant_outputs"));
    }

    /// The chroma waves of the CGA over one colour cycle of eight steps, by colour index 0 to 7 (B is bit 0, G bit 1, R
    /// bit 2): reenigne, 'CRTC emulation for MESS' (2006), typed here independently of the data file.
    const CGA_WAVES: [u8; 8] = [
        0b0000_0000, // black
        0b0000_1111, // blue
        0b0111_1000, // green
        0b0011_1100, // cyan
        0b1100_0011, // red
        0b1000_0111, // magenta
        0b1111_0000, // yellow, also the burst
        0b1111_1111, // white
    ];

    /// The fundamental of a wave of eight steps: the sum of its bits times e^(-j 2 pi k / 8), as (re, im).
    fn cga_fundamental(wave: u8) -> (f64, f64) {
        (0..8)
            .filter(|k| wave >> (7 - k) & 1 == 1)
            .fold((0.0, 0.0), |(re, im), k| {
                let a = std::f64::consts::TAU * f64::from(k) / 8.0;
                (re + a.cos(), im - a.sin())
            })
    }

    /// The four levels of reenigne's circuit simulation of the old card, in mV: Y (intensity) 0 or 1, C (chroma) 0 or 1.
    const CGA_OLD_MV: [[f64; 2]; 2] = [[416.0, 1160.0], [709.0, 1460.0]];

    /// The file's 16 colours, one per index, from the card's logic written out here: the wave of the colour bits gives the
    /// mean chroma (the share of its bits set) and, from its fundamental against the yellow wave's, the phase (the burst's
    /// 180 degrees minus the turn); the old card's level is the mean of its two intensity levels weighted by that share,
    /// the new card's the weighted sum of the chroma, R, G, B and I bits; the decode is [`decode_yuv`]. `amplitude` is the
    /// chroma vector as a share of the span.
    fn cga_composite(new_card: bool, amplitude: f64) -> [[f64; 3]; 16] {
        let level = |i: usize| -> f64 {
            let wave = CGA_WAVES[i & 7];
            let c = f64::from(wave.count_ones()) / 8.0;
            let (intensity, r, g, b) = (
                f64::from(u8::from(i & 8 != 0)),
                f64::from(u8::from(i & 4 != 0)),
                f64::from(u8::from(i & 2 != 0)),
                f64::from(u8::from(i & 1 != 0)),
            );
            if new_card {
                0.29 * c + 0.10 * r + 0.22 * g + 0.07 * b + 0.32 * intensity
            } else {
                let y = &CGA_OLD_MV[usize::from(i & 8 != 0)];
                y[0] + c * (y[1] - y[0])
            }
        };
        let (black, white) = (level(0), level(15));
        let burst = cga_fundamental(CGA_WAVES[6]);
        let burst_phase = burst.1.atan2(burst.0);
        let mut out = [[0.0; 3]; 16];
        for (i, o) in out.iter_mut().enumerate() {
            let y = (level(i) - black) / (white - black);
            let wave = CGA_WAVES[i & 7];
            let (u, v) = if matches!(i & 7, 0 | 7) {
                (0.0, 0.0)
            } else {
                let f = cga_fundamental(wave);
                // The wave leads the burst by this much; the hue on the U axis is the burst's 180 degrees minus it.
                let lead = (f.1.atan2(f.0) - burst_phase).to_degrees();
                let a = (180.0 - lead).to_radians();
                (amplitude * a.cos(), amplitude * a.sin())
            };
            *o = decode_yuv(y, u, v);
        }
        out
    }

    /// The IBM CGA composite file: four variants over 17 reference entries (the 16 canonical colours and PCem's dark
    /// yellow); each variant's 16 colours are the card's logic written out in [`cga_composite`] (the waves' phases from
    /// their fundamentals, the luma from the simulated levels or the weights, the amplitude from a square wave's
    /// fundamental), the rounded amplitudes of the file within 1e-4; the six chromatic hues stay within 20 degrees of the
    /// NTSC hues of the pure colours; the greys have no chroma; hand-derived entries.
    /// Hand derivation, new card, red (colour 4): level 0.29 x 0.5 + 0.10 = 0.245, hue 90 degrees, amplitude
    /// 2 / pi x 0.29 = 0.1846: U = 0, V = 0.1846, R = 0.245 + 0.1846 / 0.877283 = 0.4554, B = 0.245,
    /// G = (0.245 - 0.299 x 0.4554 - 0.114 x 0.245) / 0.587 = 0.1378 (decoded from the unclamped red and blue).
    #[test]
    fn the_cga_composite_file_is_the_cards_logic() {
        use std::f64::consts::PI;
        let t = for_value(crate::params::SOURCE_COLOUR_TARGET_IBM_CGA_COMPOSITE).expect("cga composite");
        assert_eq!(t.variants.len(), 4);
        assert_eq!(
            t.variants.iter().map(|v| v.id.as_str()).collect::<Vec<_>>(),
            ["old_card", "new_card", "old_card_burst_gain", "new_card_burst_gain"]
        );
        // The amplitudes: a square wave of swing p has the fundamental 2 p / pi; the old card's swing is the mean of the
        // two chroma steps of the four levels over the span, the new card's is 0.29; the burst-gain variants 20 / 100.
        let old_span = CGA_OLD_MV[1][1] - CGA_OLD_MV[0][0];
        let old_swing =
            ((CGA_OLD_MV[0][1] - CGA_OLD_MV[0][0]) + (CGA_OLD_MV[1][1] - CGA_OLD_MV[1][0])) / 2.0 / old_span;
        let amplitudes = [2.0 / PI * old_swing, 2.0 / PI * 0.29, 0.2, 0.2];
        assert!((amplitudes[0] - 0.4558).abs() < 1e-4 && (amplitudes[1] - 0.1846).abs() < 1e-4);
        for (k, v) in t.variants.iter().enumerate() {
            let Transform::Signal(s) = v.transform else {
                panic!("signal")
            };
            assert_eq!(s.count, 17, "{}", v.id);
            let want = cga_composite(k % 2 == 1, amplitudes[k]);
            for i in 0..17 {
                let colour = if i == 16 { 6 } else { i };
                assert!(
                    dist(s.colours[i], want[colour]) < 1e-3,
                    "{} entry {i}: {:?} against {:?}",
                    v.id,
                    s.colours[i],
                    want[colour]
                );
            }
            // The two variants of a card with the same luma share it: the greys are the same.
            for g in [0, 7, 8, 15] {
                assert!(close_channels(s.colours[g]), "{} grey {g}: {:?}", v.id, s.colours[g]);
            }
        }
        // Hue: decode U and V back from the colours of the unclamped variants (the new card and the burst-gain ones) and
        // compare with the file's angles and with the NTSC hue of the pure colour (within 20 degrees).
        let ntsc_hue = |rgb: [f64; 3]| {
            let y = 0.299 * rgb[0] + 0.587 * rgb[1] + 0.114 * rgb[2];
            (0.877_283 * (rgb[0] - y))
                .atan2(0.492_111 * (rgb[2] - y))
                .to_degrees()
                .rem_euclid(360.0)
        };
        let mut checked = 0;
        for k in [1, 2, 3] {
            let Transform::Signal(s) = t.variants[k].transform else {
                panic!("signal")
            };
            let pures = [
                (1, [0.0, 0.0, 1.0]),
                (2, [0.0, 1.0, 0.0]),
                (3, [0.0, 1.0, 1.0]),
                (4, [1.0, 0.0, 0.0]),
                (5, [1.0, 0.0, 1.0]),
                (6, [1.0, 1.0, 0.0]),
            ];
            for (i, pure) in pures {
                for c in [i, i + 8] {
                    // A colour that clips a channel has lost its hue to the rails.
                    if s.colours[c].iter().any(|v| *v <= 0.0 || *v >= 1.0) {
                        continue;
                    }
                    checked += 1;
                    let got = ntsc_hue(s.colours[c]);
                    let want = [0.0, 0.0, 225.0, 270.0, 90.0, 45.0, 180.0][i];
                    let off = (got - want + 540.0).rem_euclid(360.0) - 180.0;
                    assert!(off.abs() < 0.5, "variant {k} colour {c}: hue {got} against {want}");
                    let natural = (got - ntsc_hue(pure) + 540.0).rem_euclid(360.0) - 180.0;
                    assert!(
                        natural.abs() < 20.0,
                        "variant {k} colour {c}: {natural} degrees from the pure hue"
                    );
                }
            }
        }
        assert!(checked >= 28, "{checked} colours checked for hue");
        // Hand-derived: the new card's red.
        let Transform::Signal(s) = t.variants[1].transform else {
            panic!("signal")
        };
        let (r, b) = (0.245 + 0.1846 / 0.877_283, 0.245);
        let g = (0.245 - 0.299 * r - 0.114 * b) / 0.587;
        assert!(dist(s.colours[4], [r, g, b]) < 1e-3, "{:?}", s.colours[4]);
        assert!((g - 0.1378).abs() < 1e-3, "{g}");
        // The old card is far more saturated than the new one: its chromatic colours clip a channel at the rails.
        let Transform::Signal(old) = t.variants[0].transform else {
            panic!("signal")
        };
        let clipped = (1..7)
            .filter(|&i| old.colours[i].iter().any(|c| *c == 0.0 || *c == 1.0))
            .count();
        assert!(clipped >= 4, "{clipped}");
        // The first variant is the target's own.
        assert_eq!(t.transform, t.variants[0].transform);
    }

    /// True when the three channels are equal (a grey): a colour with no chroma decodes to R = G = B.
    fn close_channels(c: [f64; 3]) -> bool {
        (c[0] - c[1]).abs() < 1e-9 && (c[1] - c[2]).abs() < 1e-9
    }

    /// The wave of each colour is the yellow wave turned by whole steps (the file's phases follow from the turn), written
    /// as rotations of the string and checked against the fundamental's phase: blue 4 steps from yellow, red 2, magenta 3,
    /// cyan 6, green 7, and the means 0, 0.5 and 1 of the duty.
    #[test]
    fn the_cga_waves_are_the_yellow_wave_turned_by_whole_steps() {
        let yellow = CGA_WAVES[6];
        let want = [(1, 4), (2, 7), (3, 6), (4, 2), (5, 3), (6, 0)];
        for (i, turn) in want {
            assert_eq!(yellow.rotate_left(turn), CGA_WAVES[i], "colour {i}");
            let (y, w) = (cga_fundamental(yellow), cga_fundamental(CGA_WAVES[i]));
            let amp = |f: (f64, f64)| (f.0 * f.0 + f.1 * f.1).sqrt();
            assert!((amp(y) - amp(w)).abs() < 1e-9, "colour {i}: equal fundamentals");
            // The wave that leads by `turn` steps has the phase of the yellow wave plus turn x 45 degrees.
            let lead = (w.1.atan2(w.0) - y.1.atan2(y.0)).to_degrees().rem_euclid(360.0);
            assert!((lead - f64::from(turn) * 45.0).abs() < 1e-9, "colour {i}: {lead}");
        }
        assert_eq!(
            CGA_WAVES.map(|w| w.count_ones()),
            [0, 4, 4, 4, 4, 4, 4, 8],
            "the mean chroma is 0, 0.5 or 1"
        );
    }

    /// The recognition of the CGA composite target is that of the RGBI target: every canonical colour (and the dark
    /// yellow, which stands for colour 6) is recognised as its own, a pixel off by a little still is, the closest two
    /// references of different colours are farther apart than the threshold, a pixel with no canonical colour near passes
    /// through, and the four variants share the references.
    #[test]
    fn the_cga_composite_reference_is_the_canonical_palette() {
        let t = for_value(crate::params::SOURCE_COLOUR_TARGET_IBM_CGA_COMPOSITE).expect("cga composite");
        let Transform::Signal(s) = t.transform else {
            panic!("signal")
        };
        let canonical = [
            0x000000, 0x0000AA, 0x00AA00, 0x00AAAA, 0xAA0000, 0xAA00AA, 0xAA5500, 0xAAAAAA, 0x555555, 0x5555FF,
            0x55FF55, 0x55FFFF, 0xFF5555, 0xFF55FF, 0xFFFF55, 0xFFFFFF,
        ];
        for (i, c) in canonical.iter().enumerate() {
            let rgb = hex_rgb(*c);
            assert_eq!(s.classify(rgb), Some(i), "colour {i}");
            assert_eq!(s.apply(rgb), s.colours[i]);
            assert_eq!(s.classify(rgb.map(|v| (v + 0.02).min(1.0))), Some(i), "colour {i} off");
        }
        // PCem's dark yellow stands for colour 6: the entry 16 gives the colour 6's output.
        assert_eq!(s.classify(hex_rgb(0xAAAA00)), Some(16));
        assert_eq!(s.apply(hex_rgb(0xAAAA00)), s.colours[6]);
        assert_eq!(s.colours[16], s.colours[6]);
        let mut closest = f64::MAX;
        for i in 0..17 {
            for j in 0..17 {
                if i % 16 != j % 16 && !(i == 16 && j == 6 || i == 6 && j == 16) {
                    closest = closest.min(dist(s.reference(i), s.reference(j)));
                }
            }
        }
        assert!(closest > s.threshold, "{closest}");
        let odd = [0.0, 1.0, 0.0];
        assert_eq!(s.classify(odd), None);
        assert_eq!(s.apply(odd), odd);
        for v in &t.variants {
            let Transform::Signal(o) = v.transform else {
                panic!("signal")
            };
            assert_eq!(o.references, s.references, "{}", v.id);
        }
    }

    /// The validation of `rgb_references`: an empty list, a colour past the last, an rgb past 0xFFFFFF, a computed-reference
    /// key beside the list, a gain that is not 1.
    #[test]
    fn a_bad_cga_composite_file_is_refused() {
        let ok = SHIPPED
            .iter()
            .find(|(f, _)| *f == "ibm_cga_composite.toml")
            .expect("the cga composite file")
            .1
            .to_string();
        assert!(Target::parse("t.toml", &ok).is_ok());
        let cases = [
            (
                ok.replacen("rgb = 0xFFFFFF\ncolour = 15", "rgb = 0x1FFFFFF\ncolour = 15", 1),
                "0xFFFFFF",
            ),
            (
                ok.replacen("rgb = 0xFFFFFF\ncolour = 15", "rgb = 0xFFFFFF\ncolour = 16", 1),
                "16 colours",
            ),
            (ok.replacen("gain = 1.0", "gain = 1.1", 1), "gain and a gamma of 1"),
            (ok.replacen("gamma = 1.0", "gamma = 2.2", 1), "gain and a gamma of 1"),
            (
                ok.replacen("gain = 1.0", "gain = 1.0\namplitude = 0.3", 1),
                "replaces the computed reference",
            ),
            (
                ok.replacen("gain = 1.0", "gain = 1.0\nextra_gammas = [1.5]", 1),
                "replaces the computed reference",
            ),
            (
                ok.replace("black_index = \"K:", "black_index = \"X:"),
                "provenance.black_index",
            ),
            (ok.replacen("luma_mv = [0, 215,", "luma_mv = [215,", 1), "luma_mv"),
        ];
        for (text, key) in cases {
            let err = Target::parse("t.toml", &text).expect_err(key);
            assert!(err.contains("t.toml") && err.contains(key), "{key}: {err}");
        }
    }
}
