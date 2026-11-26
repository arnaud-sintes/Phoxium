//! Source colour targets (TODO, "A GBC target"; parameter `source_colour_target` in `params/source.toml`, data files
//! `presets/targets/`): the colours of the source as the device it came from would have shown them, applied to the
//! drive voltage R'G'B' of the native raster before anything else of the chain.
//!
//! Two kinds of transform exist (`QUESTIONS.md`, "Study: source colour targets"), both on the **device code** of the
//! source and both keeping the numbers as their source gives them:
//!
//! - [`Matrix`], the LCD handhelds (Game Boy Color, Game Boy Advance): the panel mixes the three channels. With
//!   `x_j` the device code of channel `j` over its largest value (0 to 1):
//!   `l_j = input_range x x_j^input_gamma`, `s_c = min(clamp, sum_j row_c[j] l_j)`,
//!   `V'_c = output_scale x (s_c / divisor)^(1 / output_gamma)`.
//!   The Game Boy Color is the integer formula on the 5-bit codes (`input_range` 31, no gamma, clip 960, divisor
//!   `4 x 255` for the shift by two and the 255 of the output); the Game Boy Advance is Talarubi's gamma 4.0 in, 2.2
//!   out (`input_range` 1, divisor 255, scale `255 / 280`). The integer floor of a shift is not done: the chain's own
//!   8-bit output rounds.
//! - [`Curve`], a console whose output is a per-channel table (SNES): `V'_c = table[code_c] / output_max`, the
//!   entries between two codes interpolated linearly for a colour that is not on the 32 levels. A `scale` lowers the whole
//!   output (the Neo Geo's ladder levels, [`ladder_output`], and its dark bit and shadow).
//! - [`Dac`], a console whose 3-bit channel codes leave a DAC at measured levels (Mega Drive): a piecewise linear map
//!   from the levels the emulator shows to the measured levels. It has no device code to recover: the levels are the
//!   points of the map.
//! - [`Signal`], a console whose colours are made of a composite signal (C64): the hardware colours as the drive voltages
//!   a decoder gives ([`decode_yuv`]), and the recognition of each pixel as the nearest hardware colour.
//! - [`Palette`], a monochrome source shown in four chosen colours (Super Game Boy): the luma of the source picks a
//!   position between the colours. It has no device code to recover.
//!
//! **The device code.** The source is the drive voltage `V = code / 255` (the default `drive_voltage` source
//! meaning). An emulator shows the device code `c` expanded to 8 bits by repeating its bits
//! ([`expand`]: `c << 3 | c >> 2` for 5 bits); when all three channels of a pixel are exactly such an expansion
//! ([`device_codes`]) the device codes are recovered exactly (`c = code >> (8 - bits)`). Any other pixel (a source
//! that is not quantised, a scaled picture, another expansion rule) is transformed on its colours directly, with
//! `x = V`; the two readings differ by at most 0.003 of the range (see the tests), so the choice is a matter of
//! exactness, not of look.

/// Tolerance of the recovery in 8-bit code units: a code stored in FP16 and read back is off by at most 0.07, a
/// fractional code (a scaled picture) is almost always further.
pub const RECOVERY_TOLERANCE: f64 = 0.1;

/// Entries of a [`Curve`] table: 5-bit codes.
pub const CURVE_LEN: usize = 32;

/// The device code `c` of `bits` bits (4 to 8) expanded to 8 bits by repeating its bits:
/// `c << (8 - bits) | c >> (2 bits - 8)`.
pub fn expand(c: u32, bits: u32) -> u32 {
    debug_assert!((4..=8).contains(&bits));
    (c << (8 - bits)) | (c >> (2 * bits - 8))
}

/// The device codes of the drive voltage `v` when all three channels are exactly an expansion of a `bits`-bit
/// device code (within [`RECOVERY_TOLERANCE`]), `None` otherwise (also for 8 bits, where there is nothing to recover).
pub fn device_codes(v: [f64; 3], bits: u32) -> Option<[f64; 3]> {
    if bits >= 8 {
        return None;
    }
    let mut out = [0.0; 3];
    for (o, x) in out.iter_mut().zip(v) {
        let code = (x * 255.0).clamp(0.0, 255.0);
        let r = code.round();
        if (code - r).abs() > RECOVERY_TOLERANCE {
            return None;
        }
        let c = (r as u32) >> (8 - bits);
        if expand(c, bits) != r as u32 {
            return None;
        }
        *o = f64::from(c);
    }
    Some(out)
}

/// The input the transform sees, 0 to 1 per channel: the recovered device codes over the largest code, or `v`.
fn input(v: [f64; 3], bits: u32) -> [f64; 3] {
    let max = f64::from((1u32 << bits) - 1);
    device_codes(v, bits).map_or(v, |c| c.map(|c| c / max))
}

/// A device colour matrix (see the module documentation).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Matrix {
    /// The weights, one row per output channel (R, G, B), one column per input channel, as the source gives them.
    pub rows: [[f64; 3]; 3],
    /// Bits per channel of the device code (5 for the handhelds), 4 to 8.
    pub input_bits: u32,
    /// The value the linearised input reaches at the largest code (31 when the formula works on the codes, 1 on
    /// normalised values).
    pub input_range: f64,
    /// Exponent of the input (1 for none, 4.0 for the Game Boy Advance LCD).
    pub input_gamma: f64,
    /// Largest value the weighted sum may reach, `f64::INFINITY` for none.
    pub clamp: f64,
    /// The weighted sum is divided by this before the output exponent.
    pub divisor: f64,
    /// The output is raised to `1 / output_gamma` (1 for none, 2.2 for the Game Boy Advance).
    pub output_gamma: f64,
    /// Factor applied last (1, or `255 / 280` for the Game Boy Advance).
    pub output_scale: f64,
}

impl Matrix {
    /// The largest device code, `2^input_bits - 1`.
    pub fn input_max(&self) -> f64 {
        f64::from((1u32 << self.input_bits) - 1)
    }

    /// The target colour of the drive voltage `v`.
    pub fn apply(&self, v: [f64; 3]) -> [f64; 3] {
        let x = input(v, self.input_bits);
        let l = x.map(|x| {
            self.input_range
                * if self.input_gamma == 1.0 {
                    x
                } else {
                    x.max(0.0).powf(self.input_gamma)
                }
        });
        let channel = |r: &[f64; 3]| {
            let s = (r[0] * l[0] + r[1] * l[1] + r[2] * l[2]).clamp(0.0, self.clamp);
            let y = s / self.divisor;
            self.output_scale
                * if self.output_gamma == 1.0 {
                    y
                } else {
                    y.powf(1.0 / self.output_gamma)
                }
        };
        [channel(&self.rows[0]), channel(&self.rows[1]), channel(&self.rows[2])]
    }
}

/// A per-channel table (see the module documentation): 32 entries for the 5-bit codes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Curve {
    /// The output code of each device code, as the source gives them.
    pub table: [u8; CURVE_LEN],
    /// Largest output code (255).
    pub output_max: f64,
    /// A factor on the whole output (1 for none): the Neo Geo's dark bit and shadow lower every level by a constant
    /// fraction ([`ladder_output`]), which a variant of the target states as a scale on the table.
    pub scale: f64,
}

impl Curve {
    /// The target colour of the drive voltage `v`.
    pub fn apply(&self, v: [f64; 3]) -> [f64; 3] {
        let x = input(v, 5);
        x.map(|x| {
            let pos = x.clamp(0.0, 1.0) * (CURVE_LEN - 1) as f64;
            let i = (pos.floor() as usize).min(CURVE_LEN - 2);
            let f = pos - i as f64;
            self.scale * (f64::from(self.table[i]) * (1.0 - f) + f64::from(self.table[i + 1]) * f) / self.output_max
        })
    }
}

/// The output of a resistor-ladder DAC as a fraction of the supply (the Neo Geo's palette DAC, 74HC-type outputs driving
/// one resistor per bit): each bit of `code` (bit `b` through `resistances[b]`) is driven to the supply when set and to
/// ground when clear, so the node is the divider `V = G_on / (G_all + G_pull)`, with `G` the conductances (1 / R) of the
/// set resistors (`G_on`), of all the resistors (`G_all`), and of `pulldown_ohms`, an extra resistor to ground (the Neo
/// Geo's dark bit and shadow; `None` for none). Nothing loads the node (the video amplifier's input is taken as
/// infinite). The code with every bit set and no pulldown gives 1, so the levels are relative to that white.
pub fn ladder_output(resistances: &[f64], code: u32, pulldown_ohms: Option<f64>) -> f64 {
    let g = |r: &f64| 1.0 / r;
    let on: f64 = resistances
        .iter()
        .enumerate()
        .filter(|(b, _)| code >> b & 1 == 1)
        .map(|(_, r)| g(r))
        .sum();
    let all: f64 = resistances.iter().map(g).sum();
    on / (all + pulldown_ohms.map_or(0.0, |r| 1.0 / r))
}

/// A palette of four colours for a monochrome source (the Super Game Boy, later the original Game Boy): the luma of
/// the source picks a position between the four colours. `colours[0]` is the lightest, `colours[3]` the darkest; the
/// source's own shades are taken as evenly spaced in luma (a grey emulator palette: white, light grey, dark grey,
/// black), and a luma between two shades blends the two colours linearly. Not a 5-bit recovery: the luma of any
/// colour works.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    /// The four colours, lightest first, as integer codes of `output_max`.
    pub colours: [[u8; 3]; 4],
    /// Largest code of a colour channel (31 for RGB555 words, 255 for hex colours).
    pub output_max: f64,
}

impl Palette {
    /// BT.601 luma of the drive voltage (the weights `decode` uses for a monochrome phosphor).
    pub fn luma(v: [f64; 3]) -> f64 {
        0.299 * v[0] + 0.587 * v[1] + 0.114 * v[2]
    }

    /// The target colour of the drive voltage `v`.
    pub fn apply(&self, v: [f64; 3]) -> [f64; 3] {
        let pos = (1.0 - Self::luma(v).clamp(0.0, 1.0)) * 3.0;
        let i = (pos.floor() as usize).min(2);
        let f = pos - i as f64;
        let channel = |c: usize| {
            (f64::from(self.colours[i][c]) * (1.0 - f) + f64::from(self.colours[i + 1][c]) * f) / self.output_max
        };
        [channel(0), channel(1), channel(2)]
    }
}

/// Codes of a [`Dac`]: 3 bits per channel.
pub const DAC_LEN: usize = 8;

/// A console whose 3-bit channel codes leave a DAC at measured levels (Mega Drive): what the emulator shows for the
/// code `i` is the 8-bit level `input[i]` (an emulator choice: a linear step of 34, 33 or 32 for the three that are
/// documented), what the console outputs is `output[i] / output_max`. A level between two input levels (a scaled
/// picture, the Mega Drive's shadow and highlight half steps) is mapped on the straight line between the two points, a
/// level below the first or above the last input level takes the nearest output.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Dac {
    /// The 8-bit level the emulator shows for each code, strictly increasing.
    pub input: [u8; DAC_LEN],
    /// The output code of each code, as the source gives them, not decreasing.
    pub output: [u8; DAC_LEN],
    /// Largest output code (255).
    pub output_max: f64,
}

impl Dac {
    /// The target colour of the drive voltage `v`.
    pub fn apply(&self, v: [f64; 3]) -> [f64; 3] {
        v.map(|x| {
            let p = (x * 255.0).clamp(f64::from(self.input[0]), f64::from(self.input[DAC_LEN - 1]));
            let i = (0..DAC_LEN - 1)
                .find(|&i| p <= f64::from(self.input[i + 1]))
                .unwrap_or(DAC_LEN - 2);
            let (x0, x1) = (f64::from(self.input[i]), f64::from(self.input[i + 1]));
            let t = (p - x0) / (x1 - x0);
            (f64::from(self.output[i]) * (1.0 - t) + f64::from(self.output[i + 1]) * t) / self.output_max
        })
    }
}

/// Colours a [`Signal`] holds at most (the NES has 64, the Atari 8-bit GTIA 256: 16 hues of 16 luma levels).
pub const SIGNAL_MAX: usize = 256;

/// The colour-difference scales of [`decode_yuv`]: `U = U_PER_B_Y (B' - Y)` and `V = V_PER_R_Y (R' - Y)`. A chip that gives
/// R' - Y and B' - Y directly (the TMS9928A) is decoded as `decode_yuv(y, U_PER_B_Y b_y, V_PER_R_Y r_y)`, which is
/// `R' = Y + r_y`, `B' = Y + b_y` and G' from the BT.601 luma.
pub const U_PER_B_Y: f64 = 0.492_111;
pub const V_PER_R_Y: f64 = 0.877_283;

/// The drive voltages R'G'B' a television's decoder gives for a luma `y` (0 black, 1 white) and colour differences
/// `u`, `v` on the scale of the composite model (`U = 0.492111 (B' - Y)`, `V = 0.877283 (R' - Y)`, BT.601 luma),
/// clamped to the rails [0, 1]: the matrix of `crate::composite`, so a colour made here goes through the composite
/// model and returns unchanged.
pub fn decode_yuv(y: f64, u: f64, v: f64) -> [f64; 3] {
    let r = y + v / V_PER_R_Y;
    let b = y + u / U_PER_B_Y;
    let g = (y - 0.299 * r - 0.114 * b) / 0.587;
    [r, g, b].map(|c| c.clamp(0.0, 1.0))
}

/// A console whose colours are made of a composite signal (Commodore 64, later the NES), as the drive voltages a
/// television's decoder would give for each hardware colour ([`decode_yuv`]), and the recognition of those colours in
/// an emulator picture: each pixel is classified to the nearest hardware colour and replaced by it, a pixel with no
/// close colour passes through. An emulator's palette is a PC colour, computed for a PC display and for a chip and a
/// television setting of its own, so the pixel is compared with a **reference** palette (`references`), computed by a
/// fixed model that stands between the published ones and does not depend on the chip revision chosen for the output;
/// the reference is a classification aid, never applied to the output.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Signal {
    /// The hardware colours as drive voltages, the first `count` used.
    pub colours: [[f64; 3]; SIGNAL_MAX],
    pub count: usize,
    /// The colours an emulator shows for them, as the classification compares (the first `count` used).
    pub references: [[f64; 3]; SIGNAL_MAX],
    /// The largest distance (Euclidean, in the compared space) at which a pixel is taken for a hardware colour.
    pub threshold: f64,
}

impl Signal {
    /// The reference colour `i` (what an emulator shows for it), in the compared space.
    pub fn reference(&self, i: usize) -> [f64; 3] {
        self.references[i]
    }

    /// The nearest hardware colour of the drive voltage `v` (the first of equals), `None` when none is within the
    /// threshold.
    pub fn classify(&self, v: [f64; 3]) -> Option<usize> {
        let mut best: Option<(usize, f64)> = None;
        for i in 0..self.count {
            let r = self.reference(i);
            let d = ((v[0] - r[0]).powi(2) + (v[1] - r[1]).powi(2) + (v[2] - r[2]).powi(2)).sqrt();
            if best.is_none_or(|(_, b)| d < b) {
                best = Some((i, d));
            }
        }
        best.filter(|(_, d)| *d <= self.threshold).map(|(i, _)| i)
    }

    /// The target colour of the drive voltage `v`: the hardware colour it is taken for, else itself.
    pub fn apply(&self, v: [f64; 3]) -> [f64; 3] {
        self.classify(v).map_or(v, |i| self.colours[i])
    }
}

/// The transform of a target. A [`Signal`] is a few kilobytes of colours: the enum stays `Copy` for the cheap small kinds and
/// is only built and read, never stored in bulk.
#[derive(Clone, Copy, Debug, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum Transform {
    Matrix(Matrix),
    Curve(Curve),
    Palette(Palette),
    Dac(Dac),
    Signal(Signal),
}

impl Transform {
    /// Bits per channel of the device code.
    pub fn input_bits(&self) -> u32 {
        match self {
            Transform::Matrix(m) => m.input_bits,
            Transform::Curve(_) => 5,
            Transform::Palette(_) | Transform::Dac(_) | Transform::Signal(_) => 8,
        }
    }

    /// The target colour of the drive voltage `v`.
    pub fn apply(&self, v: [f64; 3]) -> [f64; 3] {
        match self {
            Transform::Matrix(m) => m.apply(v),
            Transform::Curve(c) => c.apply(v),
            Transform::Palette(p) => p.apply(v),
            Transform::Dac(d) => d.apply(v),
            Transform::Signal(s) => s.apply(v),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::close;

    /// The Game Boy Color numbers of the near.sh colour emulation article (weights on 5-bit codes, clip 960, shift 2).
    fn gbc() -> Matrix {
        Matrix {
            rows: [[26.0, 4.0, 2.0], [0.0, 24.0, 8.0], [6.0, 4.0, 22.0]],
            input_bits: 5,
            input_range: 31.0,
            input_gamma: 1.0,
            clamp: 960.0,
            divisor: 1020.0,
            output_gamma: 1.0,
            output_scale: 1.0,
        }
    }

    /// The Game Boy Advance formula of the same article (Talarubi): the columns here are R, G, B, so the rows of the
    /// article (written on b, g, r) are reversed.
    fn gba() -> Matrix {
        Matrix {
            rows: [[255.0, 50.0, 0.0], [10.0, 230.0, 30.0], [50.0, 10.0, 220.0]],
            input_bits: 5,
            input_range: 1.0,
            input_gamma: 4.0,
            clamp: f64::INFINITY,
            divisor: 255.0,
            output_gamma: 2.2,
            output_scale: 255.0 / 280.0,
        }
    }

    fn snes() -> Curve {
        Curve {
            table: [
                0x00, 0x01, 0x03, 0x06, 0x0a, 0x0f, 0x15, 0x1c, 0x24, 0x2d, 0x37, 0x42, 0x4e, 0x5b, 0x69, 0x78, 0x88,
                0x90, 0x98, 0xa0, 0xa8, 0xb0, 0xb8, 0xc0, 0xc8, 0xd0, 0xd8, 0xe0, 0xe8, 0xf0, 0xf8, 0xff,
            ],
            output_max: 255.0,
            scale: 1.0,
        }
    }

    fn v(c: u32) -> f64 {
        f64::from(expand(c, 5)) / 255.0
    }

    /// Hand derivation: code 31 on all channels sums to 992 for every row, clipped at 960, 960 / 1020 = 240 / 255;
    /// black stays black.
    #[test]
    fn gbc_white_is_240_of_255_and_black_is_black() {
        for c in gbc().apply([1.0; 3]) {
            assert!(close(c, 240.0 / 255.0, 1e-12), "{c}");
        }
        assert_eq!(gbc().apply([0.0; 3]), [0.0; 3]);
    }

    /// A pure red code 31: R = 26 x 31 = 806, G = 0, B = 6 x 31 = 186, each over 1020; pure blue 31: 62, 248, 682.
    #[test]
    fn gbc_primaries_leak_into_the_other_channels() {
        let [r, g, b] = gbc().apply([1.0, 0.0, 0.0]);
        assert!(close(r, 806.0 / 1020.0, 1e-12), "{r}");
        assert_eq!(g, 0.0);
        assert!(close(b, 186.0 / 1020.0, 1e-12), "{b}");
        let [r, g, b] = gbc().apply([0.0, 0.0, 1.0]);
        assert!(close(r, 62.0 / 1020.0, 1e-12) && close(g, 248.0 / 1020.0, 1e-12) && close(b, 682.0 / 1020.0, 1e-12));
    }

    /// An expanded device code is recovered exactly: code 16 expands to 132 and the sum runs on 16, not on
    /// 132 x 31 / 255. Hand derivation, grey 16 on all channels: 16 x 32 = 512 on each row, 512 / 1020 = 0.50196.
    #[test]
    fn a_five_bit_expansion_is_recovered_exactly() {
        assert_eq!(expand(16, 5), 132);
        assert_eq!(device_codes([v(16); 3], 5), Some([16.0; 3]));
        for c in gbc().apply([v(16); 3]) {
            assert!(close(c, 512.0 / 1020.0, 1e-12), "{c}");
        }
        // Mixed channels: R = 26 x 3 + 4 x 20 + 2 x 7 = 172 over 1020.
        let [r, _, _] = gbc().apply([v(3), v(20), v(7)]);
        assert!(close(r, 172.0 / 1020.0, 1e-12), "{r}");
    }

    /// A pixel that is not an expansion (one channel off by a code, or a fraction) is transformed on its colour.
    #[test]
    fn any_other_colour_is_transformed_directly() {
        assert_eq!(device_codes([v(16), v(16), 133.0 / 255.0], 5), None);
        assert_eq!(device_codes([0.4, 0.4, 0.4], 5), None);
        let [r, _, _] = gbc().apply([0.4, 0.4, 0.4]);
        assert!(close(r, 0.4 * 31.0 * 32.0 / 1020.0, 1e-12), "{r}");
        assert_eq!(device_codes([100.5 / 255.0; 3], 5), None);
        assert_eq!(device_codes([1.0; 3], 8), None, "eight bits have nothing to recover");
    }

    /// Every 5-bit code is recovered; the expansion read straight (`code / 255`) is within 0.003 of the device code
    /// over 31, at most 0.55 of an output code through the GBC matrix: the direct reading is a fallback of half a step.
    #[test]
    fn the_direct_reading_is_within_half_an_output_code() {
        let mut worst: f64 = 0.0;
        for c in 0..32u32 {
            assert_eq!(device_codes([v(c); 3], 5), Some([f64::from(c); 3]), "code {c}");
            worst = worst.max((v(c) - f64::from(c) / 31.0).abs());
        }
        assert!(worst < 0.003, "{worst}");
        assert!(worst * 26.0 * 31.0 / 4.0 < 0.55);
    }

    /// The expansion of other widths: 4 bits repeat the nibble, 8 bits are the code.
    #[test]
    fn expansion_of_other_widths() {
        assert_eq!(expand(0xA, 4), 0xAA);
        assert_eq!(expand(0xFF, 8), 0xFF);
        assert_eq!(expand(31, 5), 255);
        assert_eq!(expand(0, 5), 0);
    }

    /// Hand derivation of the Game Boy Advance: white (all 31): lr = lg = lb = 1, the rows sum to 305, 270 and 280,
    /// out = (305 / 255)^(1 / 2.2) x 255 / 280 = 0.98793 for red, (270 / 255)^(1 / 2.2) x 255 / 280 = 0.93469 for green,
    /// (280 / 255)^(1 / 2.2) x 255 / 280 = 0.95027 for blue (the 280 is the largest row sum, so blue is the channel
    /// that sets the scale of the whole); black stays black.
    #[test]
    fn gba_white_and_black() {
        let [r, g, b] = gba().apply([1.0; 3]);
        let want = |sum: f64| (sum / 255.0).powf(1.0 / 2.2) * 255.0 / 280.0;
        assert!(close(r, want(305.0), 1e-12) && close(g, want(270.0), 1e-12) && close(b, want(280.0), 1e-12));
        assert!(
            close(r, 0.98793, 1e-5) && close(g, 0.93469, 1e-5) && close(b, 0.95027, 1e-5),
            "{r} {g} {b}"
        );
        assert_eq!(gba().apply([0.0; 3]), [0.0; 3]);
    }

    /// A pure red code 31 on the Game Boy Advance: red 255 over 255 = 1, green 10 and blue 50 of 255 through the
    /// output exponent: green (10 / 255)^(1 / 2.2) x 255 / 280, blue (50 / 255)^(1 / 2.2) x 255 / 280.
    #[test]
    fn gba_red_leaks_into_green_and_blue() {
        let [r, g, b] = gba().apply([1.0, 0.0, 0.0]);
        let s = 255.0 / 280.0;
        assert!(close(r, s, 1e-12), "{r}");
        assert!(close(g, (10.0f64 / 255.0).powf(1.0 / 2.2) * s, 1e-12), "{g}");
        assert!(close(b, (50.0f64 / 255.0).powf(1.0 / 2.2) * s, 1e-12), "{b}");
    }

    /// The input gamma darkens the middle: grey code 16 (x = 16 / 31) gives lr = lg = lb = (16 / 31)^4 = 0.07096.
    #[test]
    fn gba_input_gamma_darkens_the_middle() {
        let l = (16.0f64 / 31.0).powi(4);
        let [r, _, _] = gba().apply([v(16); 3]);
        assert!(
            close(r, ((255.0 + 50.0) * l / 255.0).powf(1.0 / 2.2) * 255.0 / 280.0, 1e-12),
            "{r}"
        );
    }

    /// The SNES table: the codes map to the entries (0 to 0, 16 to 0x88, 31 to 0xff); a colour between two codes is
    /// interpolated: x = 15.5 / 31 gives the mean of the entries 15 (0x78) and 16 (0x88), 0x80.
    #[test]
    fn snes_table_and_interpolation() {
        let t = snes();
        assert_eq!(t.table.len(), CURVE_LEN);
        assert_eq!(t.apply([0.0; 3]), [0.0; 3]);
        assert_eq!(t.apply([1.0; 3]), [1.0; 3]);
        let [r, g, b] = t.apply([v(16), v(8), v(31)]);
        assert!(close(r, f64::from(0x88u8) / 255.0, 1e-12) && close(g, f64::from(0x24u8) / 255.0, 1e-12) && b == 1.0);
        let [r, _, _] = t.apply([15.5 / 31.0; 3]);
        assert!(close(r, 128.0 / 255.0, 1e-12), "{r}");
        // The upper half is nearly left alone, the lower half darkened: code 8 shows 0x24 = 36, not 66.
        assert!(close(t.apply([v(8); 3])[0], 36.0 / 255.0, 1e-12));
    }

    /// The Neo Geo ladder, 3900, 2200, 1000, 470 and 220 ohm on bits 0 to 4. Hand derivation in millisiemens: 1 / 3.9 =
    /// 0.256410, 1 / 2.2 = 0.454545, 1, 1 / 0.47 = 2.127660, 1 / 0.22 = 4.545455, total 8.384070. Code 1 is
    /// 0.256410 / 8.384070 = 0.030583 (7.80 of 255), code 2 is 0.454545 / 8.384070 = 0.054215 (13.82), code 3 their sum
    /// 0.084798 (21.62), the top code 1 (white); the weights are not binary and still rise strictly with the code.
    #[test]
    fn ladder_levels_of_the_neo_geo() {
        const R: [f64; 5] = [3900.0, 2200.0, 1000.0, 470.0, 220.0];
        let level = |c: u32| ladder_output(&R, c, None);
        assert_eq!(level(0), 0.0);
        assert!(close(level(31), 1.0, 1e-12));
        assert!(close(level(1), 0.256_410 / 8.384_070, 1e-6) && close(level(1) * 255.0, 7.80, 5e-3));
        assert!(close(level(2), 0.454_545 / 8.384_070, 1e-6) && close(level(2) * 255.0, 13.82, 5e-3));
        assert!(close(level(3), 0.084_798, 1e-6));
        assert!(close(level(16) * 255.0, 138.25, 5e-3), "{}", level(16) * 255.0);
        assert!((1..32).all(|c| level(c) > level(c - 1)), "the levels rise strictly");
    }

    /// The dark bit (8200 ohm to ground) and the shadow (150 ohm) lower every level by the same factor, whatever the code:
    /// the pulldown adds to the conductance of the divider's denominator only. Hand derivation: 8.384070 / (8.384070 +
    /// 1 / 8.2) = 8.384070 / 8.506022 = 0.985662, with the shadow 8.384070 / (8.384070 + 6.666667) = 0.557054, with both
    /// 8.384070 / 15.172689 = 0.552576.
    #[test]
    fn ladder_pulldowns_scale_every_level() {
        const R: [f64; 5] = [3900.0, 2200.0, 1000.0, 470.0, 220.0];
        for (pull, factor) in [
            (8200.0, 0.985_663),
            (150.0, 0.557_054),
            (1.0 / (1.0 / 8200.0 + 1.0 / 150.0), 0.552_576),
        ] {
            for c in 1..32 {
                let got = ladder_output(&R, c, Some(pull)) / ladder_output(&R, c, None);
                assert!(close(got, factor, 2e-6), "pulldown {pull}, code {c}: {got}");
            }
            assert_eq!(ladder_output(&R, 0, Some(pull)), 0.0, "black stays black");
        }
        // The whole of white lowered by the shadow is 0.557 of the reference white.
        assert!(close(ladder_output(&R, 31, Some(150.0)), 0.557_054, 2e-6));
    }

    /// A scale on the curve lowers every output by that factor and leaves black alone: with a scale of 0.5 the table's
    /// code 31 (248) gives 248 / 510 and the code 16 (138) gives 138 / 510.
    #[test]
    fn curve_scale_multiplies_the_output() {
        let c = Curve {
            table: std::array::from_fn(|i| if i == 16 { 138 } else { (i * 8) as u8 }),
            output_max: 255.0,
            scale: 0.5,
        };
        assert_eq!(c.apply([0.0; 3]), [0.0; 3]);
        assert!(close(c.apply([1.0; 3])[0], 248.0 / 510.0, 1e-12));
        assert!(close(c.apply([v(16); 3])[1], 138.0 / 510.0, 1e-12));
    }

    /// The Super Game Boy palette 1-A as RGB555 words 0x67BF, 0x265B, 0x10B5, 0x2866 (lightest first): red = bits 0 to 4,
    /// green = bits 5 to 9, blue = bits 10 to 14.
    fn sgb_1a() -> Palette {
        let word = |w: u32| [(w & 31) as u8, ((w >> 5) & 31) as u8, ((w >> 10) & 31) as u8];
        Palette {
            colours: [word(0x67BF), word(0x265B), word(0x10B5), word(0x2866)],
            output_max: 31.0,
        }
    }

    /// Hand derivation: 0x67BF = 0b0_11001_11101_11111: red 31, green 29, blue 25, so white is (31, 29, 25) / 31 and the
    /// darkest 0x2866 = 0b0_01010_00011_00110 is (6, 3, 10) / 31; mid positions blend linearly: luma 2/3 is the second
    /// colour 0x265B = (27, 18, 9), and 0x10B5 = (21, 5, 4).
    #[test]
    fn palette_ends_and_shades() {
        let p = sgb_1a();
        assert_eq!(p.colours, [[31, 29, 25], [27, 18, 9], [21, 5, 4], [6, 3, 10]]);
        let [r, g, b] = p.apply([1.0; 3]);
        assert!(close(r, 1.0, 1e-12) && close(g, 29.0 / 31.0, 1e-12) && close(b, 25.0 / 31.0, 1e-12));
        let [r, g, b] = p.apply([0.0; 3]);
        assert!(close(r, 6.0 / 31.0, 1e-12) && close(g, 3.0 / 31.0, 1e-12) && close(b, 10.0 / 31.0, 1e-12));
        // The four grey levels of a grey emulator palette land on the four colours exactly.
        for (i, g) in [1.0, 2.0 / 3.0, 1.0 / 3.0, 0.0].into_iter().enumerate() {
            let out = p.apply([g; 3]);
            for (c, (o, code)) in out.iter().zip(p.colours[i]).enumerate() {
                assert!(close(*o, f64::from(code) / 31.0, 1e-12), "shade {i} channel {c}");
            }
        }
    }

    /// Between two shades the colours blend linearly in luma: halfway between the lightest and the second colour is
    /// their mean; a coloured source goes by its luma only (a pure green of luma 0.587 sits between shades 1 and 2).
    #[test]
    fn palette_blends_by_luma() {
        let p = sgb_1a();
        let mid = p.apply([5.0 / 6.0; 3]);
        assert!(close(mid[0], (31.0 + 27.0) / 2.0 / 31.0, 1e-12) && close(mid[1], (29.0 + 18.0) / 2.0 / 31.0, 1e-12));
        let green = p.apply([0.0, 1.0, 0.0]);
        let pos: f64 = (1.0 - 0.587) * 3.0;
        let (i, f) = (pos.floor() as usize, pos.fract());
        let want = f64::from(p.colours[i][0]) * (1.0 - f) + f64::from(p.colours[i + 1][0]) * f;
        assert!(close(green[0], want / 31.0, 1e-12), "{green:?}");
        assert_eq!(Transform::Palette(p).input_bits(), 8);
    }

    /// The Mega Drive DAC of TmEE''s scope readings (normal mode) behind a Genesis Plus GX picture (step 34).
    fn md_gpgx() -> Dac {
        Dac {
            input: [0, 34, 68, 102, 136, 170, 204, 238],
            output: [0, 52, 87, 116, 144, 172, 206, 255],
            output_max: 255.0,
        }
    }

    /// Hand derivation: the code 3 of a Genesis Plus GX picture is 102 of 255 and goes to 116 of 255 (the third measured
    /// level); the ends are 0 and 238 (the top code of that emulator, 6.4 percent below full white) to 255.
    #[test]
    fn dac_levels_map_to_the_measured_levels() {
        let d = md_gpgx();
        for (i, (&inp, &out)) in d.input.iter().zip(d.output.iter()).enumerate() {
            let got = d.apply([f64::from(inp) / 255.0; 3]);
            assert!(
                got.iter().all(|c| close(*c, f64::from(out) / 255.0, 1e-12)),
                "code {i}: {got:?}"
            );
        }
        assert_eq!(d.apply([0.0; 3]), [0.0; 3]);
        assert_eq!(d.apply([1.0; 3]), [1.0; 3], "above the last level: the last output");
    }

    /// A level between two codes is on the straight line between the points: 85 is half way between 68 and 102, so
    /// half way between 87 and 116, 101.5; the Mega Drive''s shadow half step, 17 of a Genesis Plus GX picture, is half
    /// way between 0 and 52, 26 (the measured shadow level is 29).
    #[test]
    fn dac_blends_between_levels() {
        let d = md_gpgx();
        assert!(close(d.apply([85.0 / 255.0; 3])[0], 101.5 / 255.0, 1e-12));
        assert!(close(d.apply([17.0 / 255.0; 3])[0], 26.0 / 255.0, 1e-12));
        // The channels are mapped on their own.
        let [r, g, b] = d.apply([34.0 / 255.0, 0.0, 238.0 / 255.0]);
        assert!(close(r, 52.0 / 255.0, 1e-12) && g == 0.0 && b == 1.0);
        assert_eq!(Transform::Dac(d).input_bits(), 8);
    }

    /// Another emulator, another step: with a step of 32 (Regen) the code 7 is 224, still the last output.
    #[test]
    fn dac_input_levels_are_the_emulators() {
        let regen = Dac {
            input: [0, 32, 64, 96, 128, 160, 192, 224],
            ..md_gpgx()
        };
        assert!(close(regen.apply([224.0 / 255.0; 3])[0], 1.0, 1e-12));
        assert!(close(regen.apply([96.0 / 255.0; 3])[0], 116.0 / 255.0, 1e-12));
        // The same picture value 102 is code 3 for one emulator and between codes for the other.
        assert!(close(md_gpgx().apply([102.0 / 255.0; 3])[0], 116.0 / 255.0, 1e-12));
        assert!(regen.apply([102.0 / 255.0; 3])[0] > 116.0 / 255.0);
    }

    /// A small signal with four colours: black, white, a red and a blue, from luma and colour differences.
    fn tiny_signal() -> Signal {
        let mut colours = [[0.0; 3]; SIGNAL_MAX];
        colours[0] = decode_yuv(0.0, 0.0, 0.0);
        colours[1] = decode_yuv(1.0, 0.0, 0.0);
        colours[2] = decode_yuv(0.3, -0.05, 0.2);
        colours[3] = decode_yuv(0.1, 0.3, 0.0);
        let references = colours.map(|c| c.map(|v| v.powf(1.25)));
        Signal {
            colours,
            count: 4,
            references,
            threshold: 0.1,
        }
    }

    /// The decode matrix: a grey is grey, a colour difference of 0 gives the luma, the rails clamp. Hand derivation
    /// for Y 0.3, U 0, V 0.1: R = 0.3 + 0.1 / 0.877283 = 0.41399, B = 0.3, G = (0.3 - 0.299 R - 0.114 B) / 0.587 = 0.2419.
    #[test]
    fn decode_yuv_matrix_and_rails() {
        assert!(decode_yuv(0.5, 0.0, 0.0).iter().all(|c| close(*c, 0.5, 1e-12)));
        let [r, g, b] = decode_yuv(0.3, 0.0, 0.1);
        assert!(
            close(r, 0.3 + 0.1 / 0.877_283, 1e-12) && close(b, 0.3, 1e-12),
            "{r} {b}"
        );
        assert!(close(g, (0.3 - 0.299 * r - 0.114 * b) / 0.587, 1e-12));
        assert!(close(g, 0.2419, 1e-3), "{g}");
        // The luma of what comes out is the luma that went in (the BT.601 weights).
        assert!(close(0.299 * r + 0.587 * g + 0.114 * b, 0.3, 1e-12));
        // The rails: a colour difference too large for the gamut is clamped.
        assert_eq!(decode_yuv(0.5, 0.0, 2.0)[0], 1.0);
        assert_eq!(decode_yuv(0.0, 0.0, -2.0)[0], 0.0);
    }

    /// Every hardware colour is its own nearest in the compared space and is replaced by itself; the distance between
    /// the two closest colours is larger than the threshold; a pixel far from all of them passes through.
    #[test]
    fn signal_classifies_to_the_nearest_hardware_colour() {
        let s = tiny_signal();
        for i in 0..s.count {
            assert_eq!(s.classify(s.reference(i)), Some(i), "colour {i}");
            assert_eq!(s.apply(s.reference(i)), s.colours[i]);
        }
        let mut closest = f64::MAX;
        for i in 0..s.count {
            for j in i + 1..s.count {
                let (a, b) = (s.reference(i), s.reference(j));
                closest = closest.min(((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt());
            }
        }
        assert!(closest > s.threshold, "{closest}");
        // A pixel a little off the red is still the red; a green is far from all four and passes through.
        let r = s.reference(2).map(|c| c + 0.02);
        assert_eq!(s.classify(r), Some(2));
        let green = [0.1, 0.9, 0.1];
        assert_eq!(s.classify(green), None);
        assert_eq!(s.apply(green), green);
        assert_eq!(Transform::Signal(s).input_bits(), 8);
        // The first of two equal distances wins: a count of 0 classifies nothing.
        assert_eq!(Signal { count: 0, ..s }.classify([0.0; 3]), None);
    }

    /// The 16 measured luma levels of the Atari 800XL, in volts (Altirra Hardware Reference Manual, Table 116).
    const XL_LUMA: [f64; 16] = [
        1.112, 1.184, 1.264, 1.334, 1.408, 1.488, 1.568, 1.648, 1.688, 1.760, 1.840, 1.928, 1.992, 2.072, 2.144, 2.224,
    ];

    /// The NTSC GTIA written out from the manual's own formulas, independently of the data file: 16 hues of 16 luma levels,
    /// index `hue * 16 + luma`; the luma `levels` are normalised between brightness 0 and 15, hue 0 has no chroma, hue `h`
    /// has the U-V phase `180 - delay (h - 1)` degrees and the amplitude `saturation` (the share of the luma range).
    fn gtia(levels: &[f64; 16], saturation: f64, delay: f64, gain: f64, gamma: f64) -> Signal {
        let span = levels[15] - levels[0];
        let mut colours = [[0.0; 3]; SIGNAL_MAX];
        for hue in 0..16 {
            for luma in 0..16 {
                let y = (levels[luma] - levels[0]) / span;
                let (u, v) = if hue == 0 {
                    (0.0, 0.0)
                } else {
                    let a = (180.0 - delay * (hue as f64 - 1.0)).to_radians();
                    (saturation * a.cos(), saturation * a.sin())
                };
                colours[hue * 16 + luma] = decode_yuv(y * gain, u * gain, v * gain).map(|c| c.powf(gamma));
            }
        }
        Signal {
            colours,
            count: 256,
            references: colours,
            threshold: 0.1,
        }
    }

    /// The luma ladder of the GTIA (LUM0 36k, LUM1 18k, LUM2 9.1k, LUM3 4.7k ohm) reproduces the shape of the measured
    /// levels: the level of brightness `b` is the share of the conductance of its set bits in the total, 1/36k, 1/18k, 1/9.1k
    /// and 1/4.7k out of 406.0 (in 1e-6 siemens). Hand derivation for brightness 8 (only the 4.7k): 212.77 / 406.0 = 0.5241,
    /// measured (1.688 - 1.112) / 1.112 = 0.518, and the quirk of 7 and 8 close together: 8 - 7 is 0.048 of the range, the
    /// mean step 0.067. The model is within 0.012 of every measured level of the 800XL.
    #[test]
    fn gtia_luma_ladder_follows_the_measured_shape() {
        let g = [1.0 / 36.0, 1.0 / 18.0, 1.0 / 9.1, 1.0 / 4.7];
        let total: f64 = g.iter().sum();
        let model = |b: usize| (0..4).filter(|k| b >> k & 1 == 1).map(|k| g[k]).sum::<f64>() / total;
        assert!(close(model(8), 0.5241, 5e-4), "{}", model(8));
        assert!(close(model(15), 1.0, 1e-12) && model(0) == 0.0);
        let mut worst: f64 = 0.0;
        for (b, l) in XL_LUMA.iter().enumerate() {
            worst = worst.max((model(b) - (l - XL_LUMA[0]) / (XL_LUMA[15] - XL_LUMA[0])).abs());
        }
        println!("ladder against the 800XL measurement: worst {worst:.4}");
        assert!(worst < 0.012, "{worst}");
        assert!(model(8) - model(7) < 0.05 && 0.05 < (model(15) - model(0)) / 15.0 * 0.8);
    }

    /// Hand derivation, 800XL, hue 1 luma 8 (`$18`), saturation 23.9 percent: Y = (1.688 - 1.112) / 1.112 = 0.51799; the
    /// phase of hue 1 is 180 degrees (the burst, -U), so U = -0.239 and V = 0; R = Y = 0.51799, B = Y + U / 0.492111 =
    /// 0.03233, G = (Y - 0.299 R - 0.114 B) / 0.587 = 0.6123. A grey (hue 0) is its luma; luma 15 of hue 0 is white. Hue 3
    /// is 180 - 2 x 25.5 = 129 degrees: U = 0.239 cos 129 = -0.15040, V = 0.239 sin 129 = 0.18575.
    #[test]
    fn gtia_800xl_colours_hand_derived() {
        let s = gtia(&XL_LUMA, 0.239, 25.5, 1.0, 1.0);
        let [r, g, b] = s.colours[0x18];
        assert!(
            close(r, 0.51799, 5e-4) && close(g, 0.6123, 5e-4) && close(b, 0.03233, 5e-4),
            "{r} {g} {b}"
        );
        assert_eq!(s.colours[0x00], [0.0; 3], "hue 0 luma 0 is black");
        assert_eq!(s.colours[0x0F], [1.0; 3], "hue 0 luma 15 is white");
        let grey = (1.688 - 1.112) / 1.112;
        assert!(s.colours[0x08].iter().all(|c| close(*c, grey, 1e-12)));
        // Hue 3 luma 8: the same luma, the chroma at 129 degrees (a red-orange: R above, B below the luma).
        let [r, g, b] = s.colours[0x38];
        let (u, v) = (0.239 * 129f64.to_radians().cos(), 0.239 * 129f64.to_radians().sin());
        assert!(close(u, -0.15040, 5e-5) && close(v, 0.18575, 5e-5), "{u} {v}");
        assert!(close(r, grey + v / 0.877_283, 1e-12) && close(b, grey + u / 0.492_111, 1e-12));
        assert!(r > grey && b < grey);
        assert!(
            close(0.299 * r + 0.587 * g + 0.114 * b, grey, 1e-12),
            "the luma is kept"
        );
        // Hue 15 is 14 delays further: 180 - 14 x 25.5 = -177 degrees, 3 degrees from hue 1.
        let (a1, a15) = (180.0f64, 180.0 - 14.0 * 25.5);
        assert!(close((a15 + 360.0 - a1).abs(), 3.0, 1e-9));
    }

    /// Every colour of the computed palette is its own nearest (the references are the colours themselves here), and the
    /// palettes of the other machines and of the other ends of the hue delay (23.5 and 27.5 degrees) land on the right colour
    /// or on a neighbour that looks the same: for the machines near the reference (saturation 0.27, hue delay 25.5) the
    /// reference colour found is within 0.1 of the intended colour for every colour, for the 800 (saturation 0.35) for
    /// most; the exact index is found for a third to nine tenths (hue steps of 24 degrees are narrow, so an exact index
    /// is not what recognition promises; the counts are printed).
    #[test]
    fn gtia_palette_classifies_to_its_own_colours_and_survives_other_machines() {
        let reference = gtia(&XL_LUMA, 0.27, 25.5, 1.0, 1.0);
        for i in 0..256 {
            assert_eq!(reference.classify(reference.colours[i]), Some(i), "colour {i:#04X}");
        }
        let mut closest = f64::MAX;
        for i in 0..256 {
            for j in i + 1..256 {
                let (a, b) = (reference.colours[i], reference.colours[j]);
                closest = closest.min(((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt());
            }
        }
        println!("closest two of the 256 colours: {closest:.4}");
        assert!(closest > 0.0, "no two colours coincide");
        for (name, sat, delay) in [
            ("800XL", 0.239, 25.5),
            ("130XE", 0.233, 25.5),
            ("800", 0.350, 25.5),
            ("400", 0.306, 25.5),
            ("delay 23.5", 0.239, 23.5),
            ("delay 27.5", 0.239, 27.5),
        ] {
            let other = gtia(&XL_LUMA, sat, delay, 1.0, 1.0);
            let (mut exact, mut near, mut worst) = (0, 0, 0.0f64);
            for i in 0..256 {
                let c = other.colours[i];
                let got = reference.classify(c);
                exact += usize::from(got == Some(i));
                let d = got.map_or(f64::MAX, |j| {
                    let o = reference.colours[j];
                    ((c[0] - o[0]).powi(2) + (c[1] - o[1]).powi(2) + (c[2] - o[2]).powi(2)).sqrt()
                });
                near += usize::from(d < 0.1);
                if d < f64::MAX {
                    worst = worst.max(d);
                }
            }
            println!("{name}: {exact} of 256 exact, {near} within 0.1 of the reference found, worst {worst:.3}");
            assert!(exact >= 70 && near >= 160, "{name}: {exact} {near}");
        }
    }

    /// Y, R-Y and B-Y of seven colours of the TMS9928A (Texas Instruments data manual, November 1982, Table 2-3), in percent of
    /// white with the differences around 47: black, medium green, dark blue, light red, dark yellow, grey, white.
    const TMS: [[f64; 3]; 7] = [
        [0.0, 47.0, 47.0],
        [53.0, 7.0, 20.0],
        [40.0, 40.0, 100.0],
        [67.0, 93.0, 27.0],
        [73.0, 57.0, 7.0],
        [80.0, 47.0, 47.0],
        [100.0, 47.0, 47.0],
    ];

    /// The colour of a [`TMS`] entry as the chip's colour differences decode: `decode_yuv` with the differences scaled to U and V.
    fn tms(c: [f64; 3]) -> [f64; 3] {
        decode_yuv(
            c[0] / 100.0,
            U_PER_B_Y * (c[2] - 47.0) / 100.0,
            V_PER_R_Y * (c[1] - 47.0) / 100.0,
        )
    }

    /// R' = Y + (R-Y) and B' = Y + (B-Y) for a chip that gives the differences directly, G' from the BT.601 luma, clamped.
    /// Medium green (0.53, -0.40, -0.27): (0.13, 0.78622, 0.26). Dark blue (0.40, -0.07, 0.53): (0.33, 0.33273, 0.93). Light
    /// red (0.67, 0.46, -0.20): R' = 1.13 clamps to 1, G' = 0.47453 (from the unclamped red, which keeps the luma), B' = 0.47.
    /// Dark yellow (0.73, 0.10, -0.40): (0.83, 0.75675, 0.33). A grey and the white are their luma.
    #[test]
    fn colour_differences_at_unit_scale_decode_to_the_luma_plus_the_difference() {
        let [r, g, b] = tms(TMS[1]);
        assert!(
            close(r, 0.13, 1e-12) && close(g, 0.78622, 1e-4) && close(b, 0.26, 1e-12),
            "{r} {g} {b}"
        );
        let [r, g, b] = tms(TMS[2]);
        assert!(
            close(r, 0.33, 1e-12) && close(g, 0.33273, 1e-4) && close(b, 0.93, 1e-12),
            "{r} {g} {b}"
        );
        let [r, g, b] = tms(TMS[3]);
        assert!(
            r == 1.0 && close(g, 0.47453, 1e-4) && close(b, 0.47, 1e-12),
            "{r} {g} {b}"
        );
        let [r, g, b] = tms(TMS[4]);
        assert!(
            close(r, 0.83, 1e-12) && close(g, 0.75675, 1e-4) && close(b, 0.33, 1e-12),
            "{r} {g} {b}"
        );
        assert_eq!(tms(TMS[0]), [0.0; 3]);
        assert!(tms(TMS[5]).iter().all(|c| close(*c, 0.8, 1e-12)));
        assert!(tms(TMS[6]).iter().all(|c| close(*c, 1.0, 1e-12)));
        // The scales are the ones decode_yuv divides by, so U and V of the composite model are U_PER_B_Y (B' - Y), V_PER_R_Y (R' - Y).
        assert_eq!((U_PER_B_Y, V_PER_R_Y), (0.492_111, 0.877_283));
    }

    /// A signal with two reference sets (the colours repeated, the references at two exponents, entry `k x n + i` for the set
    /// `k` and the colour `i`, as the data file builds it): a pixel is taken for the nearest entry of either set, a colour at an
    /// exponent between the two lands on the right colour, and `apply` gives the same hardware colour for both sets.
    #[test]
    fn two_reference_sets_recognise_a_palette_at_either_exponent() {
        let n = TMS.len();
        let mut colours = [[0.0; 3]; SIGNAL_MAX];
        let mut references = [[0.0; 3]; SIGNAL_MAX];
        for (k, gamma) in [1.1, 1.5].into_iter().enumerate() {
            for (i, c) in TMS.iter().enumerate() {
                colours[k * n + i] = tms(*c);
                references[k * n + i] = tms(*c).map(|v| v.powf(gamma));
            }
        }
        let s = Signal {
            colours,
            count: 2 * n,
            references,
            threshold: 0.12,
        };
        for g in [1.0, 1.1, 1.25, 1.4, 1.5, 1.6] {
            for (i, c) in TMS.iter().enumerate() {
                let pixel = tms(*c).map(|v| v.powf(g));
                let got = s.classify(pixel).expect("recognised");
                assert_eq!(got % n, i, "colour {i} at exponent {g}");
                assert_eq!(s.apply(pixel), tms(*c));
            }
        }
        assert_eq!(s.apply(s.reference(n + 3)), s.colours[3]);
        // A pixel far from every colour passes through.
        assert_eq!(s.classify([0.0, 1.0, 0.0]), None);
    }
}
