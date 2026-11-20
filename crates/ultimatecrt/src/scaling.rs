//! Source integer scaling: recovers the native raster of a source that was upscaled by a simple
//! integer factor with nearest neighbour (for example a 256×240 game shown at 3×).
//!
//! The divisor is either given by the user or detected. Detection looks at the run lengths of
//! identical pixels along rows (horizontal factor) and of identical rows (vertical factor): a factor
//! k is kept while nearly all runs are multiples of k, and the largest surviving k wins. A divisor of
//! 1 means the source is native (x1). Smoothed or non-integer upscales are not handled: they simply
//! detect as 1.

use std::str::FromStr;

/// A rectangle inside the captured frame, in captured pixels.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    /// Clamps the rectangle to a `w` by `h` frame.
    pub fn clamp(self, w: u32, h: u32) -> Rect {
        let x = self.x.min(w);
        let y = self.y.min(h);
        Rect {
            x,
            y,
            w: self.w.min(w - x),
            h: self.h.min(h - y),
        }
    }
}

impl FromStr for Rect {
    type Err = String;
    /// "x,y,w,h"
    fn from_str(s: &str) -> Result<Self, String> {
        let v: Vec<u32> = s
            .split(',')
            .map(|p| p.trim().parse().map_err(|_| format!("bad number in \"{s}\"")))
            .collect::<Result<_, _>>()?;
        match v[..] {
            [x, y, w, h] if w > 0 && h > 0 => Ok(Rect { x, y, w, h }),
            _ => Err("expected x,y,w,h with a non-zero size".into()),
        }
    }
}

/// Parses "N" or "NXxNY" into a horizontal and vertical divisor.
pub fn parse_divisor(s: &str) -> Result<(u32, u32), String> {
    let parse = |p: &str| {
        p.parse::<u32>()
            .ok()
            .filter(|&n| n >= 1)
            .ok_or_else(|| format!("bad divisor \"{s}\""))
    };
    match s.split_once('x') {
        Some((a, b)) => Ok((parse(a)?, parse(b)?)),
        None => parse(s).map(|n| (n, n)),
    }
}

/// What the app captures and how it recovers the native raster (T-96), resolved from the crop and
/// divisor modes (`source_crop_mode`, `source_divisor_mode`) and the manual values (command line, else the
/// source profile).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SourceMap {
    /// Capture the client area (`auto`, `manual`), else the whole window (`none`).
    pub client_area: bool,
    /// Manual crop rectangle inside the captured area (`manual`).
    pub crop: Option<Rect>,
    /// Fixed divisor (`manual`; `none` gives 1 x 1), `None` = detected.
    pub divisor: Option<(u32, u32)>,
}

impl SourceMap {
    /// Resolves the modes of `params` with the manual `crop` and `divisor`. A manual mode without its value
    /// falls back to the automatic one, with a warning.
    pub fn resolve(
        params: &crate::params::Params,
        crop: Option<Rect>,
        divisor: Option<(u32, u32)>,
    ) -> (SourceMap, Vec<String>) {
        use crate::params::{
            SOURCE_CROP_MODE_MANUAL, SOURCE_CROP_MODE_NONE, SOURCE_DIVISOR_MODE_MANUAL, SOURCE_DIVISOR_MODE_NONE,
        };
        let mut warnings = Vec::new();
        let (client_area, crop) = match params.source_crop_mode {
            SOURCE_CROP_MODE_NONE => (false, None),
            SOURCE_CROP_MODE_MANUAL => {
                if crop.is_none() {
                    warnings.push("crop mode manual without a crop rectangle: the client area is used".to_string());
                }
                (true, crop)
            }
            _ => (true, None),
        };
        let divisor = match params.source_divisor_mode {
            SOURCE_DIVISOR_MODE_NONE => Some((1, 1)),
            SOURCE_DIVISOR_MODE_MANUAL => {
                if divisor.is_none() {
                    warnings.push("divisor mode manual without a divisor: the divisor is detected".to_string());
                }
                divisor
            }
            _ => None,
        };
        (
            SourceMap {
                client_area,
                crop,
                divisor,
            },
            warnings,
        )
    }
}

/// Largest manual source scaling multiplier offered in the Source section.
pub const MAX_MULTIPLIER: u32 = 10;

/// The "Source scaling multiplier" of the Source section: the integer factor the game was upscaled by in the
/// captured window. It is the divisor mode (`source_divisor_mode`) and the manual divisor (`--divisor`, else the
/// source profile) seen as one choice.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Multiplier {
    /// Detected from the captured frames (`auto`, the default).
    #[default]
    Auto,
    /// The capture is the native raster (`none`, divisor 1).
    None,
    /// Forced (`manual`): horizontal and vertical factor, 1 to [`MAX_MULTIPLIER`] (equal when linked).
    Manual(u32, u32),
}

impl Multiplier {
    /// The multiplier the parameters and the manual divisor in force give (as [`SourceMap::resolve`] reads
    /// them: a manual mode without a divisor is the automatic one).
    pub fn of(params: &crate::params::Params, divisor: Option<(u32, u32)>) -> Multiplier {
        use crate::params::{SOURCE_DIVISOR_MODE_MANUAL, SOURCE_DIVISOR_MODE_NONE};
        match (params.source_divisor_mode, divisor) {
            (SOURCE_DIVISOR_MODE_NONE, _) => Multiplier::None,
            (SOURCE_DIVISOR_MODE_MANUAL, Some((x, y))) => Multiplier::Manual(x, y),
            _ => Multiplier::Auto,
        }
    }

    /// A forced value with both factors within 1 to [`MAX_MULTIPLIER`].
    pub fn manual(x: u32, y: u32) -> Multiplier {
        Multiplier::Manual(x.clamp(1, MAX_MULTIPLIER), y.clamp(1, MAX_MULTIPLIER))
    }

    /// The fixed divisor of the source map (`None`: detected).
    pub fn divisor(self) -> Option<(u32, u32)> {
        match self {
            Multiplier::Auto => None,
            Multiplier::None => Some((1, 1)),
            Multiplier::Manual(x, y) => Some((x, y)),
        }
    }

    /// Whether X and Y are the same factor.
    pub fn linked(self) -> bool {
        !matches!(self, Multiplier::Manual(x, y) if x != y)
    }

    /// Writes the choice to the parameters and the source profile (mode, divisor, vertical divisor when it
    /// differs). The caller drops any command-line divisor, so the menu's choice wins for the session.
    pub fn apply(self, params: &mut crate::params::Params, profile: &mut crate::settings::SourceProfile) {
        use crate::params::{SOURCE_DIVISOR_MODE_AUTO, SOURCE_DIVISOR_MODE_MANUAL, SOURCE_DIVISOR_MODE_NONE};
        let (mode, name, divisor) = match self {
            Multiplier::Auto => (SOURCE_DIVISOR_MODE_AUTO, "auto", None),
            Multiplier::None => (SOURCE_DIVISOR_MODE_NONE, "none", None),
            Multiplier::Manual(x, y) => (SOURCE_DIVISOR_MODE_MANUAL, "manual", Some((x, y))),
        };
        params.source_divisor_mode = mode;
        profile.divisor_mode = Some(name.to_string());
        profile.divisor = divisor.map(|d| d.0);
        profile.divisor_v = divisor.filter(|d| d.0 != d.1).map(|d| d.1);
    }

    /// The factor in words: `x3`, `x2 × 3`.
    pub fn pair_text(d: (u32, u32)) -> String {
        if d.0 == d.1 {
            format!("x{}", d.0)
        } else {
            format!("x{} \u{00D7} {}", d.0, d.1)
        }
    }

    /// The hint shown when a forced value disagrees with the detection: `detected x3, forced x2`.
    pub fn disagreement(self, detected: Option<(u32, u32)>) -> Option<String> {
        let forced = self.divisor()?;
        let detected = detected?;
        (forced != detected).then(|| {
            format!(
                "detected {}, forced {}",
                Multiplier::pair_text(detected),
                Multiplier::pair_text(forced)
            )
        })
    }
}

/// Informative frames analysed before the detected divisor is locked (unless x1 is certain earlier).
/// A flat frame (a black boot screen, a fade) is not informative and does not count.
const DETECT_FRAMES: u32 = 30;
/// Largest upscale factor considered.
const MAX_K: usize = 32;
/// Pixels ignored on every side: Windows 11 rounds the corners of a window, which would otherwise
/// leave odd-length runs in the first and last rows.
const MARGIN: u32 = 16;
/// A factor is accepted if at most this fraction of the analysed pixels is not in a run that is a
/// multiple of it.
const TOLERANCE: f64 = 0.01;

pub struct Detector {
    cand_x: [bool; MAX_K + 1],
    cand_y: [bool; MAX_K + 1],
    frames: u32,
    locked: bool,
}

impl Default for Detector {
    fn default() -> Self {
        Detector {
            cand_x: [true; MAX_K + 1],
            cand_y: [true; MAX_K + 1],
            frames: 0,
            locked: false,
        }
    }
}

/// Marks the factors that do not explain the run lengths `runs` (first and last run dropped,
/// they are cut by the analysed area). Returns false when `runs` carries no information (fewer
/// than three runs: a flat line says nothing about the scale).
fn eliminate(cand: &mut [bool; MAX_K + 1], runs: &[u32]) -> bool {
    if runs.len() < 3 {
        return false;
    }
    let runs = &runs[1..runs.len() - 1];
    let total: u64 = runs.iter().map(|&r| r as u64).sum();
    for (k, candidate) in cand.iter_mut().enumerate().skip(2) {
        let bad: u64 = runs
            .iter()
            .filter(|&&r| !(r as usize).is_multiple_of(k))
            .map(|&r| r as u64)
            .sum();
        if bad as f64 > TOLERANCE * total as f64 {
            *candidate = false;
        }
    }
    true
}

fn best(cand: &[bool; MAX_K + 1]) -> u32 {
    (2..=MAX_K).rev().find(|&k| cand[k]).unwrap_or(1) as u32
}

impl Detector {
    pub fn reset(&mut self) {
        *self = Detector::default();
    }

    pub fn is_locked(&self) -> bool {
        self.locked
    }

    /// Divisor to use: (1, 1) until the detection is locked.
    pub fn divisor(&self) -> (u32, u32) {
        if self.locked {
            (best(&self.cand_x), best(&self.cand_y))
        } else {
            (1, 1)
        }
    }

    /// Analyses `region` of a BGRA8 frame that is `frame_w` pixels wide. Input that does not hold the region (a
    /// buffer too short for it, a region outside the frame's width, an empty frame) is not analysed: returns false
    /// (not ready), and nothing changes. True when the input was usable (analysed, or nothing left to analyse).
    pub fn feed(&mut self, data: &[u8], frame_w: u32, region: Rect) -> bool {
        if !region_fits(data.len(), frame_w, region) {
            return false;
        }
        let m = MARGIN.min(region.w / 8).min(region.h / 8);
        if self.locked || region.w <= 2 * m + 2 || region.h <= 2 * m + 2 {
            return true;
        }
        let (x0, x1) = (region.x + m, region.x + region.w - m);
        let (y0, y1) = (region.y + m, region.y + region.h - m);
        let at = |y: u32, x: u32| (y as usize * frame_w as usize + x as usize) * 4;
        let row = |y: u32| -> &[u8] { &data[at(y, x0)..at(y, x1)] };

        let mut runs = Vec::new();
        let mut vertical = Vec::new();
        let mut vertical_run = 0u32;
        let mut informative = false;
        for y in y0..y1 {
            let current = row(y);
            // Horizontal runs of identical pixels, evaluated per row.
            runs.clear();
            let mut run = 0u32;
            let mut previous: Option<&[u8; 4]> = None;
            for px in current.as_chunks::<4>().0 {
                if previous == Some(px) {
                    run += 1;
                } else {
                    if run > 0 {
                        runs.push(run);
                    }
                    run = 1;
                    previous = Some(px);
                }
            }
            runs.push(run);
            informative |= eliminate(&mut self.cand_x, &runs);

            // Vertical runs of identical rows.
            if y > y0 && row(y - 1) == current {
                vertical_run += 1;
            } else {
                if vertical_run > 0 {
                    vertical.push(vertical_run);
                }
                vertical_run = 1;
            }
        }
        vertical.push(vertical_run);
        informative |= eliminate(&mut self.cand_y, &vertical);

        if !informative {
            return true;
        }
        self.frames += 1;
        let any = (2..=MAX_K).any(|k| self.cand_x[k] || self.cand_y[k]);
        self.locked = !any || self.frames >= DETECT_FRAMES;
        true
    }
}

/// Whether a BGRA8 buffer of `len` bytes, `frame_w` pixels wide, holds every pixel of `region` (not empty, inside the
/// frame's width, its last row within the buffer). Computed without overflow for any input.
pub fn region_fits(len: usize, frame_w: u32, region: Rect) -> bool {
    let (x, y, w, h, fw) = (
        region.x as u128,
        region.y as u128,
        region.w as u128,
        region.h as u128,
        frame_w as u128,
    );
    // The byte after the region's last pixel: ((y + h - 1) * fw + x + w) * 4, in u128 (no overflow for u32 input).
    w > 0 && h > 0 && x + w <= fw && ((y + h - 1) * fw + x + w) * 4 <= len as u128
}

/// The part of the captured frame that is the source, and how it maps to native pixels.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct View {
    pub origin: (u32, u32),
    pub div: (u32, u32),
    pub native: (u32, u32),
}

impl View {
    pub fn full(w: u32, h: u32) -> Self {
        View {
            origin: (0, 0),
            div: (1, 1),
            native: (w, h),
        }
    }

    pub fn new(region: Rect, div: (u32, u32)) -> Self {
        let native = (region.w / div.0, region.h / div.1);
        // A divisor that leaves almost nothing (for example on a flat frame) is not credible.
        let (div, native) = if native.0 < 8 || native.1 < 8 {
            ((1, 1), (region.w, region.h))
        } else {
            (div, native)
        };
        View {
            origin: (region.x, region.y),
            div,
            native,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame of `w`×`h` native pixels with varied colours, upscaled by (kx, ky).
    fn frame(w: u32, h: u32, kx: u32, ky: u32) -> (Vec<u8>, u32, u32) {
        let (fw, fh) = (w * kx, h * ky);
        let mut data = Vec::new();
        for y in 0..fh {
            for x in 0..fw {
                let (nx, ny) = (x / kx, y / ky);
                let v = ((nx * 7 + ny * 13) % 251) as u8;
                data.extend_from_slice(&[v, v ^ 0x55, v.wrapping_mul(3), 255]);
            }
        }
        (data, fw, fh)
    }

    fn detect(w: u32, h: u32, kx: u32, ky: u32) -> (u32, u32) {
        let (data, fw, fh) = frame(w, h, kx, ky);
        let mut d = Detector::default();
        for _ in 0..DETECT_FRAMES {
            d.feed(
                &data,
                fw,
                Rect {
                    x: 0,
                    y: 0,
                    w: fw,
                    h: fh,
                },
            );
        }
        d.divisor()
    }

    #[test]
    fn detects_integer_upscales() {
        assert_eq!(detect(64, 48, 3, 3), (3, 3));
        assert_eq!(detect(64, 48, 4, 4), (4, 4));
        assert_eq!(detect(64, 48, 2, 3), (2, 3));
    }

    #[test]
    fn native_source_locks_to_one_immediately() {
        let (data, fw, fh) = frame(64, 48, 1, 1);
        let mut d = Detector::default();
        d.feed(
            &data,
            fw,
            Rect {
                x: 0,
                y: 0,
                w: fw,
                h: fh,
            },
        );
        assert!(d.is_locked());
        assert_eq!(d.divisor(), (1, 1));
    }

    #[test]
    fn tolerates_rounded_window_corners() {
        let (mut data, fw, fh) = frame(64, 48, 3, 3);
        // Black out the bottom corners like a rounded window: odd-length runs on the last rows.
        for y in fh - 9..fh {
            let cut = (fh - y) as usize * 3;
            for x in 0..cut.min(fw as usize) {
                let i = (y as usize * fw as usize + x) * 4;
                data[i..i + 4].copy_from_slice(&[0, 0, 0, 0]);
                let j = (y as usize * fw as usize + (fw as usize - 1 - x)) * 4;
                data[j..j + 4].copy_from_slice(&[0, 0, 0, 0]);
            }
        }
        let mut d = Detector::default();
        for _ in 0..DETECT_FRAMES {
            d.feed(
                &data,
                fw,
                Rect {
                    x: 0,
                    y: 0,
                    w: fw,
                    h: fh,
                },
            );
        }
        assert_eq!(d.divisor(), (3, 3));
    }

    #[test]
    fn flat_frames_never_lock() {
        let (fw, fh) = (192u32, 144u32);
        let data = vec![0u8; (fw * fh * 4) as usize];
        let mut d = Detector::default();
        for _ in 0..DETECT_FRAMES * 2 {
            d.feed(
                &data,
                fw,
                Rect {
                    x: 0,
                    y: 0,
                    w: fw,
                    h: fh,
                },
            );
        }
        assert!(!d.is_locked());
        assert_eq!(d.divisor(), (1, 1));
        // A real frame afterwards still detects the scale.
        let (data, fw, fh) = frame(64, 48, 3, 3);
        for _ in 0..DETECT_FRAMES {
            d.feed(
                &data,
                fw,
                Rect {
                    x: 0,
                    y: 0,
                    w: fw,
                    h: fh,
                },
            );
        }
        assert_eq!(d.divisor(), (3, 3));
    }

    /// T-96: the crop and divisor modes. Defaults: client area, detected divisor (as before T-96).
    #[test]
    fn source_map_modes() {
        use crate::params::{ParamValue, Params};
        let r = Rect {
            x: 1,
            y: 2,
            w: 30,
            h: 40,
        };
        let mut p = Params::default();
        let (m, w) = SourceMap::resolve(&p, Some(r), Some((2, 2)));
        assert_eq!((m.client_area, m.crop, m.divisor), (true, None, None));
        assert!(w.is_empty());
        assert!(crate::settings::set_enum_by_name(&mut p, "source_crop_mode", "manual"));
        assert!(crate::settings::set_enum_by_name(
            &mut p,
            "source_divisor_mode",
            "manual"
        ));
        let (m, w) = SourceMap::resolve(&p, Some(r), Some((2, 3)));
        assert_eq!((m.client_area, m.crop, m.divisor), (true, Some(r), Some((2, 3))));
        assert!(w.is_empty());
        let (m, w) = SourceMap::resolve(&p, None, None);
        assert_eq!((m.client_area, m.crop, m.divisor), (true, None, None));
        assert_eq!(w.len(), 2);
        assert!(p.set(
            "source_crop_mode",
            ParamValue::Enum(crate::params::SOURCE_CROP_MODE_NONE)
        ));
        assert!(crate::settings::set_enum_by_name(&mut p, "source_divisor_mode", "none"));
        let (m, _) = SourceMap::resolve(&p, Some(r), Some((2, 2)));
        assert_eq!((m.client_area, m.crop, m.divisor), (false, None, Some((1, 1))));
    }

    /// The Source section's multiplier against the divisor mode and value: auto, none, manual (linked or per
    /// axis), the profile round trip and the source map it gives.
    #[test]
    fn multiplier_state_mapping() {
        use crate::params::{Params, SOURCE_DIVISOR_MODE_AUTO, SOURCE_DIVISOR_MODE_MANUAL, SOURCE_DIVISOR_MODE_NONE};
        use crate::settings::SourceProfile;
        let p = Params::default();
        assert_eq!(p.source_divisor_mode, SOURCE_DIVISOR_MODE_AUTO);
        assert_eq!(
            Multiplier::of(&p, Some((3, 3))),
            Multiplier::Auto,
            "auto ignores a divisor"
        );
        for (m, mode, profile_div, profile_v) in [
            (Multiplier::Auto, SOURCE_DIVISOR_MODE_AUTO, None, None),
            (Multiplier::None, SOURCE_DIVISOR_MODE_NONE, None, None),
            (Multiplier::Manual(3, 3), SOURCE_DIVISOR_MODE_MANUAL, Some(3), None),
            (Multiplier::Manual(2, 4), SOURCE_DIVISOR_MODE_MANUAL, Some(2), Some(4)),
        ] {
            let mut params = Params::default();
            let mut profile = SourceProfile {
                exe: "game.exe".into(),
                class: "C".into(),
                divisor: Some(7),
                ..SourceProfile::default()
            };
            m.apply(&mut params, &mut profile);
            assert_eq!(params.source_divisor_mode, mode, "{m:?}");
            assert_eq!((profile.divisor, profile.divisor_v), (profile_div, profile_v), "{m:?}");
            // Live: the multiplier read back from the parameters and the profile's divisor.
            assert_eq!(Multiplier::of(&params, profile.divisor_pair()), m);
            // The profile alone, applied at the next attach, gives the same choice and the same source map.
            let mut fresh = Params::default();
            assert!(profile.apply_to(&mut fresh).is_empty());
            assert_eq!(Multiplier::of(&fresh, profile.divisor_pair()), m);
            let (map, warnings) = SourceMap::resolve(&fresh, None, profile.divisor_pair());
            assert!(warnings.is_empty());
            assert_eq!(map.divisor, m.divisor());
            // Through the settings file.
            let mut s = crate::settings::Settings::default();
            s.upsert_source(profile.clone());
            let back = crate::settings::Settings::from_toml(&s.to_toml().unwrap()).unwrap();
            assert_eq!(back.find_source("game.exe", "C"), Some(&profile));
        }
        assert!(Multiplier::Manual(3, 3).linked() && Multiplier::Auto.linked());
        assert!(!Multiplier::Manual(2, 3).linked());
        assert_eq!(Multiplier::manual(0, 12), Multiplier::Manual(1, MAX_MULTIPLIER));
        assert_eq!(
            Multiplier::Manual(2, 2).disagreement(Some((3, 3))).as_deref(),
            Some("detected x3, forced x2")
        );
        assert_eq!(Multiplier::Manual(3, 3).disagreement(Some((3, 3))), None);
        assert_eq!(Multiplier::Auto.disagreement(Some((3, 3))), None);
        assert_eq!(Multiplier::Manual(2, 2).disagreement(None), None);
        assert_eq!(
            Multiplier::None.disagreement(Some((2, 3))).as_deref(),
            Some("detected x2 \u{00D7} 3, forced x1")
        );
    }

    #[test]
    fn parses_options() {
        assert_eq!(parse_divisor("3"), Ok((3, 3)));
        assert_eq!(parse_divisor("2x4"), Ok((2, 4)));
        assert!(parse_divisor("0").is_err());
        assert_eq!(
            "1,2,30,40".parse::<Rect>(),
            Ok(Rect {
                x: 1,
                y: 2,
                w: 30,
                h: 40
            })
        );
    }
}
