//! Named colour sets: primaries, white points and the standards that pair them (TODO Low, "Primaries and
//! white-point presets").
//!
//! A set is a starting point, not a mode: choosing one writes the four chromaticities of the parameter table
//! (`tube_red_xy`, `tube_green_xy`, `tube_blue_xy`, `tube_white_xy`, `params/colour.toml`), which stay editable
//! and keep travelling in presets, profiles and the offline `render` command. What a combo shows is derived, not
//! stored: the name of the set the current values equal (to 1e-4), or "Custom". Nothing can go stale.
//!
//! Every number is from `docs/CRT.md` ("Chromaticity (CIE 1931 xy)" and "White point"), with its tag there.
//! The picture only shows them with "Match sRGB colours" off (`output_match_srgb`, the tube-faithful matrix) or
//! with the white retune of the gun on; the panel says so.

use crate::params::{ParamValue, Params};

/// A set of three phosphor chromaticities (CIE 1931 xy).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Primaries {
    pub name: &'static str,
    pub red: [f32; 2],
    pub green: [f32; 2],
    pub blue: [f32; 2],
}

/// A white point (CIE 1931 xy).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct White {
    pub name: &'static str,
    pub xy: [f32; 2],
}

/// Primaries and white together, as a standard or a tube is specified.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Standard {
    pub name: &'static str,
    pub primaries: usize,
    pub white: usize,
}

/// The primaries (CRT.md, "Standards" and "Measured or vendor-stated tubes").
pub const PRIMARIES: &[Primaries] = &[
    Primaries {
        name: "SMPTE-C (RP 145 / 170M)",
        red: [0.630, 0.340],
        green: [0.310, 0.595],
        blue: [0.155, 0.070],
    },
    Primaries {
        name: "EBU (Tech 3213)",
        red: [0.640, 0.330],
        green: [0.290, 0.600],
        blue: [0.150, 0.060],
    },
    Primaries {
        name: "Rec.709 / sRGB",
        red: [0.640, 0.330],
        green: [0.300, 0.600],
        blue: [0.150, 0.060],
    },
    Primaries {
        name: "NTSC 1953",
        red: [0.670, 0.330],
        green: [0.210, 0.710],
        blue: [0.140, 0.080],
    },
    Primaries {
        name: "NTSC-J (ARIB TR-B9)",
        red: [0.618, 0.350],
        green: [0.280, 0.605],
        blue: [0.152, 0.063],
    },
    Primaries {
        name: "Trinitron (GDM-17SE1)",
        red: [0.621, 0.340],
        green: [0.281, 0.606],
        blue: [0.152, 0.067],
    },
    Primaries {
        name: "P22 measured (SMPTE J. 1982)",
        red: [0.635, 0.340],
        green: [0.305, 0.595],
        blue: [0.155, 0.070],
    },
];

/// The white points (CRT.md, "White point").
pub const WHITES: &[White] = &[
    White {
        name: "D65 (6504 K)",
        xy: [0.3127, 0.3290],
    },
    White {
        name: "Illuminant C (NTSC 1953)",
        xy: [0.3101, 0.3162],
    },
    White {
        name: "About 7500 K (RCA ColorTrak)",
        xy: [0.301, 0.308],
    },
    White {
        name: "D93 (9300 K + 8 MPCD)",
        xy: [0.2831, 0.2971],
    },
    White {
        name: "9300 K + 27 MPCD (Japanese TV)",
        xy: [0.281, 0.311],
    },
    White {
        name: "9300 K (Trinitron GDM-17SE1)",
        xy: [0.2836, 0.2963],
    },
];

/// The standards: primaries and white as one choice (indices into [`PRIMARIES`] and [`WHITES`]).
pub const STANDARDS: &[Standard] = &[
    Standard {
        name: "SMPTE-C, D65 (NTSC studio)",
        primaries: 0,
        white: 0,
    },
    Standard {
        name: "EBU, D65 (PAL studio)",
        primaries: 1,
        white: 0,
    },
    Standard {
        name: "Rec.709 / sRGB, D65",
        primaries: 2,
        white: 0,
    },
    Standard {
        name: "NTSC 1953, Illuminant C",
        primaries: 3,
        white: 1,
    },
    Standard {
        name: "NTSC-J, D93 (Japanese TV, 9300 K)",
        primaries: 4,
        white: 3,
    },
    Standard {
        name: "Trinitron GDM-17SE1, 9300 K",
        primaries: 5,
        white: 5,
    },
];

/// How close two chromaticities must be to count as the same set (the table holds 3 to 4 decimals).
const SAME: f32 = 1e-4;

fn near(a: [f32; 2], b: [f32; 2]) -> bool {
    (a[0] - b[0]).abs() <= SAME && (a[1] - b[1]).abs() <= SAME
}

/// The index of the primaries set the parameters equal, if any.
#[must_use]
pub fn current_primaries(p: &Params) -> Option<usize> {
    PRIMARIES
        .iter()
        .position(|s| near(s.red, p.tube_red_xy) && near(s.green, p.tube_green_xy) && near(s.blue, p.tube_blue_xy))
}

/// The index of the white point the parameters equal, if any.
#[must_use]
pub fn current_white(p: &Params) -> Option<usize> {
    WHITES.iter().position(|w| near(w.xy, p.tube_white_xy))
}

/// The index of the standard (primaries and white together) the parameters equal, if any.
#[must_use]
pub fn current_standard(p: &Params) -> Option<usize> {
    let (pi, wi) = (current_primaries(p)?, current_white(p)?);
    STANDARDS.iter().position(|s| s.primaries == pi && s.white == wi)
}

/// Writes the primaries set `index` (a no-op for an index out of range). True when a value changed.
pub fn apply_primaries(p: &mut Params, index: usize) -> bool {
    let Some(s) = PRIMARIES.get(index) else {
        return false;
    };
    let mut changed = false;
    for (id, xy) in [
        ("tube_red_xy", s.red),
        ("tube_green_xy", s.green),
        ("tube_blue_xy", s.blue),
    ] {
        changed |= write(p, id, xy);
    }
    changed
}

/// Stores `xy` in the parameter `id`; true when the stored value differs from before (`Params::set` only says
/// that the value was accepted).
fn write(p: &mut Params, id: &str, xy: [f32; 2]) -> bool {
    let before = p.get(id);
    p.set(id, ParamValue::Vec2(xy));
    p.get(id) != before
}

/// Writes the white point `index`. True when the value changed.
pub fn apply_white(p: &mut Params, index: usize) -> bool {
    WHITES.get(index).is_some_and(|w| write(p, "tube_white_xy", w.xy))
}

/// Writes the standard `index`: its primaries and its white. True when a value changed.
pub fn apply_standard(p: &mut Params, index: usize) -> bool {
    let Some(s) = STANDARDS.get(index) else {
        return false;
    };
    let a = apply_primaries(p, s.primaries);
    let b = apply_white(p, s.white);
    a || b
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The numbers of CRT.md, read off the tables, for the sets the TODO names (EBU, SMPTE-C, NTSC-J 9300 K,
    /// Trinitron): a typo in a coordinate fails here.
    #[test]
    fn the_sets_are_the_crt_md_values() {
        let by = |n: &str| *PRIMARIES.iter().find(|s| s.name.starts_with(n)).unwrap();
        assert_eq!(by("EBU").green, [0.290, 0.600], "EBU green, not the Rec.709 0.30");
        assert_eq!(by("SMPTE-C").red, [0.630, 0.340]);
        assert_eq!(by("NTSC-J").red, [0.618, 0.350]);
        assert_eq!(by("Trinitron").green, [0.281, 0.606]);
        let w = |n: &str| WHITES.iter().find(|s| s.name.starts_with(n)).unwrap().xy;
        assert_eq!(w("D65"), [0.3127, 0.3290]);
        assert_eq!(w("D93"), [0.2831, 0.2971]);
        assert_eq!(w("9300 K + 27"), [0.281, 0.311]);
    }

    /// Every set is a real gamut: a triangle with area, the white inside it (the gun retune and the output
    /// matrix need that), and no two sets share a name.
    #[test]
    fn every_set_is_a_usable_gamut() {
        let area = |s: &Primaries| {
            0.5 * ((s.green[0] - s.red[0]) * (s.blue[1] - s.red[1]) - (s.blue[0] - s.red[0]) * (s.green[1] - s.red[1]))
        };
        let sign =
            |a: [f32; 2], b: [f32; 2], c: [f32; 2]| (a[0] - c[0]) * (b[1] - c[1]) - (b[0] - c[0]) * (a[1] - c[1]);
        for s in PRIMARIES {
            assert!(area(s).abs() > 0.05, "{}", s.name);
            for w in WHITES {
                let (d1, d2, d3) = (
                    sign(w.xy, s.red, s.green),
                    sign(w.xy, s.green, s.blue),
                    sign(w.xy, s.blue, s.red),
                );
                let inside = !((d1 < 0.0 || d2 < 0.0 || d3 < 0.0) && (d1 > 0.0 || d2 > 0.0 || d3 > 0.0));
                assert!(inside, "{} does not hold {}", s.name, w.name);
            }
        }
        for (i, a) in PRIMARIES.iter().enumerate() {
            assert!(PRIMARIES[i + 1..].iter().all(|b| b.name != a.name));
        }
        for s in STANDARDS {
            assert!(s.primaries < PRIMARIES.len() && s.white < WHITES.len(), "{}", s.name);
        }
    }

    /// The default parameters are SMPTE-C with D65, which is the first standard; applying a standard writes its
    /// four values and `current_*` reads them back; an edit makes the sets "Custom" (None).
    #[test]
    fn applying_a_set_round_trips_and_an_edit_makes_it_custom() {
        let mut p = Params::default();
        assert_eq!(current_primaries(&p), Some(0));
        assert_eq!(current_white(&p), Some(0));
        assert_eq!(current_standard(&p), Some(0));
        assert!(!apply_standard(&mut p, 0), "the default is already standard 0");
        for (i, s) in STANDARDS.iter().enumerate() {
            apply_standard(&mut p, i);
            assert_eq!(current_standard(&p), Some(i), "{}", s.name);
            assert_eq!(p.tube_red_xy, PRIMARIES[s.primaries].red);
            assert_eq!(p.tube_blue_xy, PRIMARIES[s.primaries].blue);
            assert_eq!(p.tube_white_xy, WHITES[s.white].xy);
        }
        // NTSC-J 9300 K: the ARIB primaries and the D93 white.
        apply_standard(&mut p, 4);
        assert_eq!((p.tube_red_xy, p.tube_white_xy), ([0.618, 0.350], [0.2831, 0.2971]));
        // The white alone keeps the primaries, so the standard becomes Custom but the primaries stay named.
        assert!(apply_white(&mut p, 0));
        assert_eq!(current_primaries(&p), Some(4));
        assert_eq!(current_white(&p), Some(0));
        assert_eq!(current_standard(&p), None);
        // An edit of one coordinate: Custom.
        p.tube_green_xy[0] += 0.01;
        assert_eq!(current_primaries(&p), None);
        // Out-of-range indices change nothing.
        assert!(!apply_primaries(&mut p, 99) && !apply_white(&mut p, 99) && !apply_standard(&mut p, 99));
    }
}
