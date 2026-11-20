//! Interlace, manual field mode (T-32, QUESTIONS SRC-7; CRT.md "Line modes and interlace", "Scanline
//! structure", "Flicker"). The f64 oracle of what the `screen` slot draws in one output refresh when the
//! user declares the source interlaced (`params/interlace.toml`, `shaders/include/beam.glsl`).
//!
//! Physics (CRT.md):
//! - 2:1 interlace (480i / 576i): a frame is two fields, each a full-height raster of half the lines,
//!   the second field's lines landing half a field line pitch lower, between the first field's lines. [S]
//! - The phosphor has decayed before the next field, so the fields blend in the eye only: a detail one
//!   line high is lit in one field only and twitters at half the field rate. [S][E] The decay between
//!   fields is the persistence stage's job (T-41), not this module's.
//! - 240p ("double strike"): every field lands on the same lines, the gaps are never filled. [S] That is
//!   the progressive raster of today (`off`), or a locked field of a woven frame (below).
//!
//! Model (manual, because a window capture carries no field parity, SRC-7):
//! - `off`: progressive, the raster of `beam.glsl` (T-36a): native rows x m lines of pitch P centred in
//!   the tube, line n draws native row n / m. Nothing changes.
//! - `alternate_frames_are_fields`: each captured frame is one field (for instance 240 rows of a 480i
//!   picture). The field's raster is the progressive one; field 1 (bottom) is shifted down by P / 2, half
//!   its own line pitch. Field 0 (top) keeps the progressive position exactly, so a locked top field is
//!   the 240p double-strike picture of today. [D] Convention [E]: a real tube centres the 2N-line
//!   interlaced raster, which would put field 0 a quarter of P higher than the progressive raster; that
//!   quarter-line shift of the whole picture is left out.
//! - `split_woven_frame`: the captured frame is a woven (combed) frame of N rows; field f draws the rows
//!   of parity f, rows f, f + 2, f + 4, ..., at the progressive positions of those rows. As a raster: lines
//!   of pitch 2P (twice the progressive pitch) from first + f P, line n drawing row 2 (n / m) + f. Each
//!   field line deposits its current times its own pitch 2P per unit length (one field line pitch of
//!   screen per line, the rule of `crt_model::beam::scanline_row_mean`), so one field of a flat field has
//!   the mean of the flat field, like a 240p frame: a field is a complete picture-height scan. [D]
//! - The field shown at output frame k: `alternate` gives k mod 2 with top-field-first (field 0 at even
//!   frames), the opposite with bottom-field-first; `top` or `bottom` locks the field (inspection; with a
//!   woven frame a locked field is the 240p double-strike look of that field). The output frame counter
//!   is the pipeline's `frame_index` (u32, wraps; its parity is what counts). [D]
//! - The output refresh should equal the field rate (SRC-7: 60 Hz output for a 59.94 Hz field rate):
//!   [`rate_notice`] says when it does not.

/// How the source rows map onto the tube raster (`interlace_mode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldMode {
    /// Progressive: every refresh draws every row (today's behaviour).
    Off,
    /// Each captured frame is one field; odd fields are shifted down by half a line pitch.
    AlternateFramesAreFields,
    /// The captured frame is a woven frame; each refresh draws the rows of one parity.
    SplitWovenFrame,
}

/// Which field comes first in time (`interlace_field_order`), used by [`FieldSelect::Alternate`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldOrder {
    /// Top field (field 0, the upper lines) at even output frames.
    TopFirst,
    /// Bottom field (field 1) at even output frames.
    BottomFirst,
}

/// The field shown (`interlace_field_select`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldSelect {
    /// The field alternates every output frame, in the field order.
    Alternate,
    /// Locked to the top field (field 0).
    Top,
    /// Locked to the bottom field (field 1).
    Bottom,
}

/// The top field (upper lines, even rows of a woven frame).
pub const TOP: u32 = 0;
/// The bottom field (lower lines, odd rows of a woven frame).
pub const BOTTOM: u32 = 1;

/// The interlace settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Interlace {
    pub mode: FieldMode,
    pub order: FieldOrder,
    pub select: FieldSelect,
}

impl Interlace {
    /// Progressive (the default).
    pub const OFF: Interlace = Interlace {
        mode: FieldMode::Off,
        order: FieldOrder::TopFirst,
        select: FieldSelect::Alternate,
    };

    /// Whether the progressive raster is drawn unchanged.
    pub fn is_off(&self) -> bool {
        self.mode == FieldMode::Off
    }

    /// The field ([`TOP`] or [`BOTTOM`]) shown at output frame `frame_index` (only its parity matters).
    pub fn field(&self, frame_index: u32) -> u32 {
        match self.select {
            FieldSelect::Top => TOP,
            FieldSelect::Bottom => BOTTOM,
            FieldSelect::Alternate => {
                let f = frame_index & 1;
                match self.order {
                    FieldOrder::TopFirst => f,
                    FieldOrder::BottomFirst => f ^ 1,
                }
            }
        }
    }

    /// The raster drawn for `field` from the progressive raster `progressive` (see the module notes).
    ///
    /// # Panics
    /// Unless `field` is [`TOP`] or [`BOTTOM`].
    pub fn field_raster(&self, progressive: &Raster, field: u32) -> Raster {
        assert!(field <= BOTTOM, "interlace: field {field}");
        let f = field as usize;
        match self.mode {
            FieldMode::Off => progressive.clone(),
            FieldMode::AlternateFramesAreFields => Raster {
                first: progressive.first + 0.5 * f as f64 * progressive.pitch,
                ..progressive.clone()
            },
            FieldMode::SplitWovenFrame => Raster {
                first: progressive.first + f as f64 * progressive.pitch,
                pitch: 2.0 * progressive.pitch,
                // Rows of parity f among rows 0 .. rows - 1.
                rows: (progressive.rows + 1 - f) / 2,
                mult: progressive.mult,
                row_step: 2 * progressive.row_step,
                row_phase: progressive.row_phase + f * progressive.row_step,
            },
        }
    }

    /// The raster drawn at output frame `frame_index`.
    pub fn frame_raster(&self, progressive: &Raster, frame_index: u32) -> Raster {
        if self.is_off() {
            return progressive.clone();
        }
        self.field_raster(progressive, self.field(frame_index))
    }

    /// How many progressive line pitches separate two lines of one field: 2 for a woven frame split into
    /// fields, 1 otherwise (the scanline structure one refresh shows, used by the headroom readout).
    pub fn field_pitch_factor(&self) -> f64 {
        if self.mode == FieldMode::SplitWovenFrame {
            2.0
        } else {
            1.0
        }
    }
}

/// A scanline raster as `beam.glsl` draws it (`BeamRaster`): `rows` source rows drawn `mult` times each,
/// line n centred at `first + n * pitch` and drawing source row `(n / mult) * row_step + row_phase`.
/// Lengths in one unit (host pixels in the GPU tests).
#[derive(Clone, Debug, PartialEq)]
pub struct Raster {
    pub first: f64,
    pub pitch: f64,
    /// Source rows drawn (lines = rows x mult).
    pub rows: usize,
    pub mult: usize,
    pub row_step: usize,
    pub row_phase: usize,
}

impl Raster {
    /// The progressive raster of `beam.glsl` (T-36a): `rows` x `mult` lines of pitch `pitch` centred in a
    /// raster `height` high.
    ///
    /// # Panics
    /// Unless `pitch` > 0 and `mult` >= 1.
    pub fn progressive(rows: usize, mult: usize, pitch: f64, height: f64) -> Self {
        assert!(pitch > 0.0 && mult >= 1, "raster: pitch {pitch}, mult {mult}");
        let lines = (rows * mult) as f64;
        Self {
            first: 0.5 * (height - lines * pitch) + 0.5 * pitch,
            pitch,
            rows,
            mult,
            row_step: 1,
            row_phase: 0,
        }
    }

    /// Number of lines drawn.
    pub fn lines(&self) -> usize {
        self.rows * self.mult
    }

    /// Source row drawn by line `n`.
    pub fn row(&self, n: usize) -> usize {
        (n / self.mult) * self.row_step + self.row_phase
    }

    /// Centre of line `n`.
    pub fn centre(&self, n: usize) -> f64 {
        self.first + n as f64 * self.pitch
    }

    /// The source rows drawn, once each, top to bottom.
    pub fn source_rows(&self) -> Vec<usize> {
        (0..self.rows).map(|k| self.row(k * self.mult)).collect()
    }

    /// The rows this raster draws gathered from `currents` (whole source rows of `samples` values, one
    /// after the other): a progressive image of [`Raster::rows`] rows whose row k is source row
    /// `row(k * mult)`. With `first`, `pitch` and `mult` it is the input of the progressive oracles
    /// (`crt_model::beam::AmpRaster`, `scanline_row_mean`).
    ///
    /// # Panics
    /// Unless `currents` holds whole rows and every drawn row exists.
    pub fn gather(&self, currents: &[f64], samples: usize) -> Vec<f64> {
        assert!(
            samples > 0 && currents.len().is_multiple_of(samples),
            "gather: {} values, {samples} per row",
            currents.len()
        );
        let have = currents.len() / samples;
        self.source_rows()
            .into_iter()
            .flat_map(|r| {
                assert!(r < have, "gather: row {r} of {have}");
                currents[r * samples..(r + 1) * samples].iter().copied()
            })
            .collect()
    }
}

/// Relative difference between the output refresh and the field rate above which [`rate_notice`] warns:
/// 5 % [E] (60 Hz against 59.94 Hz is 0.1 %; 50 against 59.94 is 17 %).
pub const RATE_TOLERANCE: f64 = 0.05;
/// Nominal field rates of 480i and 576i, Hz [S] (CRT.md "Line modes and interlace").
pub const FIELD_RATES_HZ: [f64; 2] = [59.94, 50.0];
/// Below this capture rate the source is taken as static (the capture delivers frames only on change),
/// and the field rate of `alternate_frames_are_fields` is unknown [E].
pub const STATIC_CAPTURE_HZ: f64 = 1.0;

fn differs(a: f64, b: f64) -> bool {
    (a - b).abs() > RATE_TOLERANCE * b
}

/// A one-line notice when the output refresh does not match the field rate (PLAN T-32), `None` when it
/// does or cannot be told. `capture_hz` and `output_hz` are the measured rates over the last second.
/// - `alternate_frames_are_fields`: the field rate is the capture rate; a mismatch repeats or drops
///   fields, which breaks the alternation (a repeated field shows on the other field's lines). Not judged
///   while the source is static (capture below [`STATIC_CAPTURE_HZ`]).
/// - `split_woven_frame`: the output shows one field per refresh, so the field rate is the output
///   refresh; it is compared with the broadcast rates [`FIELD_RATES_HZ`] (twitter and flicker run at half
///   the output refresh).
pub fn rate_notice(mode: FieldMode, capture_hz: f64, output_hz: f64) -> Option<String> {
    if !(output_hz.is_finite() && output_hz > 0.0) {
        return None;
    }
    match mode {
        FieldMode::Off => None,
        FieldMode::AlternateFramesAreFields => {
            (capture_hz.is_finite() && capture_hz >= STATIC_CAPTURE_HZ && differs(capture_hz, output_hz)).then(|| {
                format!("interlace: fields arrive at {capture_hz:.1} Hz but the output refreshes at {output_hz:.1} Hz")
            })
        }
        FieldMode::SplitWovenFrame => FIELD_RATES_HZ
            .iter()
            .all(|&r| differs(output_hz, r))
            .then(|| format!("interlace: output at {output_hz:.1} Hz, not a 59.94 or 50 Hz field rate")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::beam::{scanline_row_mean, ScanlineProfile};
    use crate::close;

    const SPLIT: Interlace = Interlace {
        mode: FieldMode::SplitWovenFrame,
        order: FieldOrder::TopFirst,
        select: FieldSelect::Alternate,
    };
    const ALT: Interlace = Interlace {
        mode: FieldMode::AlternateFramesAreFields,
        ..SPLIT
    };

    #[test]
    fn progressive_raster_matches_beam_glsl() {
        // 240 rows at 6 px per line fill 1440 px: line n at (n + 0.5) 6.
        let r = Raster::progressive(240, 1, 6.0, 1440.0);
        assert_eq!(r.lines(), 240);
        assert!(close(r.centre(0), 3.0, 1e-12) && close(r.centre(239), 1437.0, 1e-12));
        assert_eq!(r.row(17), 17);
        let m = Raster::progressive(200, 2, 3.0, 1200.0);
        assert_eq!(
            (m.lines(), m.row(0), m.row(1), m.row(2), m.row(399)),
            (400, 0, 0, 1, 199)
        );
    }

    #[test]
    fn off_is_the_progressive_raster_at_every_frame() {
        let p = Raster::progressive(240, 1, 6.0, 1440.0);
        for k in [0, 1, 2, 7, u32::MAX] {
            assert_eq!(Interlace::OFF.frame_raster(&p, k), p);
        }
        assert_eq!(Interlace::OFF.field_pitch_factor(), 1.0);
    }

    #[test]
    fn split_woven_480_rows_gives_even_rows_then_odd_rows() {
        // PLAN T-32 accept: a woven 480-row frame -> field 0 rows 0, 2, 4, ..., field 1 rows 1, 3, ...
        let p = Raster::progressive(480, 1, 3.0, 1440.0);
        let f0 = SPLIT.field_raster(&p, TOP);
        let f1 = SPLIT.field_raster(&p, BOTTOM);
        let r0 = f0.source_rows();
        let r1 = f1.source_rows();
        assert_eq!(r0.len(), 240);
        assert_eq!(r1.len(), 240);
        assert_eq!(&r0[..4], &[0, 2, 4, 6]);
        assert_eq!(&r1[..4], &[1, 3, 5, 7]);
        assert!(r0.iter().all(|r| r % 2 == 0) && r1.iter().all(|r| r % 2 == 1));
        // Complementary: together every row once.
        let mut all: Vec<usize> = r0.iter().chain(&r1).copied().collect();
        all.sort_unstable();
        assert_eq!(all, (0..480).collect::<Vec<_>>());
        // Each field line sits at the progressive position of its row.
        for (f, fr) in [(0, &f0), (1, &f1)] {
            for n in [0, 1, 100, 239] {
                let row = fr.row(n);
                assert_eq!(row % 2, f);
                assert!(close(fr.centre(n), p.centre(row), 1e-9), "field {f} line {n}");
            }
        }
        // Field pitch twice the progressive one; field 1 lies half a field pitch below field 0.
        assert!(close(f0.pitch, 6.0, 1e-12) && close(f1.first - f0.first, f0.pitch / 2.0, 1e-12));
        assert_eq!(SPLIT.field_pitch_factor(), 2.0);
    }

    #[test]
    fn split_woven_odd_row_count() {
        let p = Raster::progressive(5, 1, 2.0, 10.0);
        assert_eq!(SPLIT.field_raster(&p, TOP).source_rows(), vec![0, 2, 4]);
        assert_eq!(SPLIT.field_raster(&p, BOTTOM).source_rows(), vec![1, 3]);
        let one = Raster::progressive(1, 1, 2.0, 2.0);
        assert_eq!(SPLIT.field_raster(&one, BOTTOM).lines(), 0);
    }

    #[test]
    fn alternate_frames_shift_the_bottom_field_by_half_a_pitch() {
        let p = Raster::progressive(240, 1, 6.0, 1440.0);
        let f0 = ALT.field_raster(&p, TOP);
        let f1 = ALT.field_raster(&p, BOTTOM);
        assert_eq!(f0, p, "top field = progressive (240p double strike)");
        for n in [0, 120, 239] {
            assert!(close(f1.centre(n) - f0.centre(n), 3.0, 1e-12));
            assert_eq!(f1.row(n), n);
        }
        assert_eq!(ALT.field_pitch_factor(), 1.0);
    }

    #[test]
    fn field_alternation_and_lock() {
        let fields = |il: Interlace| (0..4).map(|k| il.field(k)).collect::<Vec<_>>();
        assert_eq!(fields(SPLIT), vec![0, 1, 0, 1]);
        let bff = Interlace {
            order: FieldOrder::BottomFirst,
            ..SPLIT
        };
        assert_eq!(fields(bff), vec![1, 0, 1, 0]);
        for order in [FieldOrder::TopFirst, FieldOrder::BottomFirst] {
            let top = Interlace {
                select: FieldSelect::Top,
                order,
                ..SPLIT
            };
            let bottom = Interlace {
                select: FieldSelect::Bottom,
                order,
                ..SPLIT
            };
            assert_eq!(fields(top), vec![0; 4]);
            assert_eq!(fields(bottom), vec![1; 4]);
        }
        // Only the parity counts, also across the u32 wrap.
        assert_eq!(SPLIT.field(u32::MAX), 1);
        assert_eq!(SPLIT.field(u32::MAX.wrapping_add(1)), 0);
        // Consecutive frames: complementary rasters.
        let p = Raster::progressive(480, 1, 3.0, 1440.0);
        let a = SPLIT.frame_raster(&p, 10).source_rows();
        let b = SPLIT.frame_raster(&p, 11).source_rows();
        assert!(a.iter().all(|r| !b.contains(r)));
        assert_eq!(a.len() + b.len(), 480);
    }

    /// A flat field keeps its mean in each field: every field line deposits current x field pitch.
    #[test]
    fn one_field_of_a_flat_field_keeps_its_mean() {
        let p = Raster::progressive(16, 1, 6.0, 96.0);
        let profile = ScanlineProfile::gaussian(1.5);
        for il in [SPLIT, ALT] {
            for f in [TOP, BOTTOM] {
                let r = il.field_raster(&p, f);
                let ones = vec![1.0; r.lines()];
                // Over the middle 8 field pitches (away from the raster ends).
                let y0 = r.first + 2.0 * r.pitch - 0.5 * r.pitch;
                let y1 = y0
                    + if il.mode == FieldMode::SplitWovenFrame {
                        4.0
                    } else {
                        8.0
                    } * r.pitch;
                let mean = scanline_row_mean(&ones, r.first, r.pitch, &profile, y0, y1);
                assert!(close(mean, 1.0, 1e-9), "{:?} field {f}: {mean}", il.mode);
            }
        }
    }

    /// PLAN T-32 accept (oracle side): a one-row detail of a woven frame is lit in one field only, a
    /// two-row detail in both.
    #[test]
    fn one_line_detail_twitters_two_line_detail_does_not() {
        let p = Raster::progressive(16, 1, 6.0, 96.0);
        let profile = ScanlineProfile::gaussian(1.2);
        let lit = |rows: &[usize], f: u32| {
            let r = SPLIT.field_raster(&p, f);
            let mut img = vec![0.0; 16];
            rows.iter().for_each(|&k| img[k] = 1.0);
            let cur = r.gather(&img, 1);
            // Light over the detail's rows (8 .. 10 progressive lines: 48 .. 60 px).
            scanline_row_mean(&cur, r.first, r.pitch, &profile, 48.0, 60.0)
        };
        let (one0, one1) = (lit(&[9], TOP), lit(&[9], BOTTOM));
        assert!(one0 < 1e-3 && one1 > 0.3, "1-line detail: top {one0}, bottom {one1}");
        let (two0, two1) = (lit(&[8, 9], TOP), lit(&[8, 9], BOTTOM));
        assert!(two0 > 0.3 && two1 > 0.3, "2-line detail: top {two0}, bottom {two1}");
        assert!(
            close(two0, two1, 0.02),
            "both fields light the 2-line detail alike: {two0} {two1}"
        );
    }

    #[test]
    fn gather_takes_the_field_rows() {
        let p = Raster::progressive(4, 1, 1.0, 4.0);
        let img = [0.0, 0.1, 1.0, 1.1, 2.0, 2.1, 3.0, 3.1];
        assert_eq!(SPLIT.field_raster(&p, BOTTOM).gather(&img, 2), vec![1.0, 1.1, 3.0, 3.1]);
        assert_eq!(p.gather(&img, 2), img.to_vec());
    }

    #[test]
    fn rate_notices() {
        use FieldMode::*;
        assert_eq!(rate_notice(Off, 30.0, 60.0), None);
        assert_eq!(rate_notice(AlternateFramesAreFields, 59.94, 60.0), None);
        assert!(rate_notice(AlternateFramesAreFields, 30.0, 60.0).is_some());
        assert!(rate_notice(AlternateFramesAreFields, 60.0, 144.0).is_some());
        assert_eq!(rate_notice(AlternateFramesAreFields, 0.0, 60.0), None, "static source");
        assert_eq!(rate_notice(SplitWovenFrame, 0.0, 60.0), None);
        assert_eq!(rate_notice(SplitWovenFrame, 30.0, 50.0), None);
        assert!(rate_notice(SplitWovenFrame, 30.0, 120.0).is_some());
        assert!(rate_notice(SplitWovenFrame, 30.0, 75.0).is_some());
        assert_eq!(rate_notice(SplitWovenFrame, 30.0, 0.0), None);
    }
}
