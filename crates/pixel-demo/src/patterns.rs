//! Extra test contents: bars, ramp, checker, motion, noise and text. Pure drawing into a
//! `w * h` buffer of 0x00RRGGBB pixels, unit tested on known pixels.

use crate::art::{fill, rgb};

const BAR_COLORS: [u32; 8] = [
    rgb(255, 255, 255),
    rgb(255, 255, 0),
    rgb(0, 255, 255),
    rgb(0, 255, 0),
    rgb(255, 0, 255),
    rgb(255, 0, 0),
    rgb(0, 0, 255),
    rgb(0, 0, 0),
];

/// Full-frame colour bars: the upper 3/4 at 100 %, the lower quarter at 50 %.
pub fn draw_bars(buf: &mut [u32], w: usize, h: usize) {
    let split = (h * 3 / 4) as i32;
    for (i, c) in BAR_COLORS.iter().enumerate() {
        let x0 = (w * i / 8) as i32;
        let x1 = (w * (i + 1) / 8) as i32;
        fill(buf, w, h, x0, 0, x1 - x0, split, *c);
        let half = |shift: u32| (((*c >> shift) & 0xFF) / 2) as u8;
        fill(
            buf,
            w,
            h,
            x0,
            split,
            x1 - x0,
            h as i32 - split,
            rgb(half(16), half(8), half(0)),
        );
    }
}

/// Four horizontal ramps 0..255 across the width: grey, red, green, blue (a quarter of the height each).
pub fn draw_ramp(buf: &mut [u32], w: usize, h: usize) {
    for y in 0..h {
        let band = (y * 4 / h).min(3);
        for x in 0..w {
            let v = if w > 1 { (x * 255 / (w - 1)) as u8 } else { 0 };
            buf[y * w + x] = match band {
                0 => rgb(v, v, v),
                1 => rgb(v, 0, 0),
                2 => rgb(0, v, 0),
                _ => rgb(0, 0, v),
            };
        }
    }
}

/// Checkerboards with cells of 1, 2, 4 and 8 pixels in the four quadrants (white and black).
pub fn draw_checker(buf: &mut [u32], w: usize, h: usize) {
    let (hw, hh) = (w / 2, h / 2);
    for y in 0..h {
        for x in 0..w {
            let (cell, lx, ly) = match (x >= hw, y >= hh) {
                (false, false) => (1, x, y),
                (true, false) => (2, x - hw, y),
                (false, true) => (4, x, y - hh),
                (true, true) => (8, x - hw, y - hh),
            };
            buf[y * w + x] = if (lx / cell + ly / cell) % 2 == 0 {
                rgb(255, 255, 255)
            } else {
                rgb(0, 0, 0)
            };
        }
    }
}

/// Motion test: a 4 px white vertical bar crossing the width in two seconds and a 16 px box
/// bouncing over the height once per second, on a dim 16 px grid. Both positions are a pure
/// function of the frame number and the rate, so a capture identifies the frame.
pub fn draw_motion(buf: &mut [u32], w: usize, h: usize, frame: u64, fps: u32) {
    fill(buf, w, h, 0, 0, w as i32, h as i32, rgb(0, 0, 0));
    for x in (0..w as i32).step_by(16) {
        fill(buf, w, h, x, 0, 1, h as i32, rgb(32, 32, 32));
    }
    for y in (0..h as i32).step_by(16) {
        fill(buf, w, h, 0, y, w as i32, 1, rgb(32, 32, 32));
    }
    let bx = (frame * w as u64 / (2 * fps as u64) % w as u64) as i32;
    fill(buf, w, h, bx, 0, 4, h as i32, rgb(255, 255, 255));
    let span = h.saturating_sub(16).max(1) as u64;
    let phase = frame * h as u64 / (fps as u64) % (2 * span);
    let by = if phase < span { phase } else { 2 * span - phase } as i32;
    fill(buf, w, h, (w as i32 - 16) / 2, by, 16, 16, rgb(255, 200, 0));
}

/// Alignment grid: black, dim lines every 8 native pixels, white lines every 32 and a white
/// one-pixel outline on the image edges (crop and divisor detection see every edge).
pub fn draw_grid(buf: &mut [u32], w: usize, h: usize) {
    for y in 0..h {
        for x in 0..w {
            let edge = x == 0 || y == 0 || x + 1 == w || y + 1 == h;
            buf[y * w + x] = if edge || x % 32 == 0 || y % 32 == 0 {
                rgb(255, 255, 255)
            } else if x % 8 == 0 || y % 8 == 0 {
                rgb(64, 64, 64)
            } else {
                rgb(0, 0, 0)
            };
        }
    }
}

/// Corner marker colours: top-left, top-right, bottom-left, bottom-right.
pub const MARKER_COLORS: [u32; 4] = [rgb(255, 0, 0), rgb(0, 255, 0), rgb(0, 0, 255), rgb(255, 255, 0)];

/// Side of the corner markers in native pixels: a sixteenth of the smaller side, 1 to 16.
pub fn marker_size(w: usize, h: usize) -> usize {
    (w.min(h) / 16).clamp(1, 16).min(w.min(h))
}

/// Solid squares in the four corners of the native image (red, green, blue, yellow from the top
/// left, clockwise reading order), so a capture shows where every corner of the picture went.
pub fn draw_markers(buf: &mut [u32], w: usize, h: usize) {
    let s = marker_size(w, h) as i32;
    let (wi, hi) = (w as i32, h as i32);
    let at = [(0, 0), (wi - s, 0), (0, hi - s), (wi - s, hi - s)];
    for ((x, y), c) in at.into_iter().zip(MARKER_COLORS) {
        fill(buf, w, h, x, y, s, s, c);
    }
}

/// Fill of the bars around the picture: plain colour or a pattern that tests surround detection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarPattern {
    None,
    Checker,
    Stripes,
    Noise,
}

impl BarPattern {
    pub const ALL: [BarPattern; 4] = [
        BarPattern::None,
        BarPattern::Checker,
        BarPattern::Stripes,
        BarPattern::Noise,
    ];

    pub fn name(self) -> &'static str {
        match self {
            BarPattern::None => "none",
            BarPattern::Checker => "checker",
            BarPattern::Stripes => "stripes",
            BarPattern::Noise => "noise",
        }
    }

    pub fn parse(s: &str) -> Option<BarPattern> {
        BarPattern::ALL.into_iter().find(|p| p.name() == s)
    }

    pub fn next(self) -> BarPattern {
        let i = BarPattern::ALL.iter().position(|p| *p == self).unwrap_or(0);
        BarPattern::ALL[(i + 1) % BarPattern::ALL.len()]
    }
}

/// Second colour of a bar pattern: each channel 64 codes away from `color` (up if dark, down if light).
pub fn contrast(color: u32) -> u32 {
    let ch = |s: u32| {
        let c = (color >> s) & 0xFF;
        (if c < 128 { c + 64 } else { c - 64 }) << s
    };
    ch(16) | ch(8) | ch(0)
}

/// Tile of a bar pattern (`w`, `h`, pixels), repeated over the bars by a GDI pattern brush.
/// `None` for the plain colour.
pub fn bar_tile(kind: BarPattern, color: u32) -> Option<(usize, usize, Vec<u32>)> {
    let color = color & 0x00FF_FFFF;
    let alt = contrast(color);
    match kind {
        BarPattern::None => None,
        BarPattern::Checker => Some((
            8,
            8,
            (0..64)
                .map(|i| {
                    if ((i % 8) / 4 + (i / 8) / 4) % 2 == 0 {
                        color
                    } else {
                        alt
                    }
                })
                .collect(),
        )),
        BarPattern::Stripes => Some((
            8,
            8,
            (0..64)
                .map(|i| if (i % 8 + i / 8) % 8 < 4 { color } else { alt })
                .collect(),
        )),
        BarPattern::Noise => {
            let mut rng = Rng::new(0xBA25);
            Some((
                64,
                64,
                (0..64 * 64)
                    .map(|_| if rng.next_u32() & 1 == 0 { color } else { alt })
                    .collect(),
            ))
        }
    }
}

/// Deterministic xorshift32 generator (hash seeding so that seed 0 works).
pub struct Rng(u32);

impl Rng {
    pub fn new(seed: u32) -> Self {
        let mut z = seed.wrapping_add(0x9E37_79B9);
        z = (z ^ (z >> 16)).wrapping_mul(0x85EB_CA6B);
        z = (z ^ (z >> 13)).wrapping_mul(0xC2B2_AE35);
        z ^= z >> 16;
        Self(if z == 0 { 0x1234_5678 } else { z })
    }
    pub fn next_u32(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }
}

/// Random RGB noise, fully determined by `seed`.
pub fn draw_noise(buf: &mut [u32], seed: u32) {
    let mut rng = Rng::new(seed);
    for p in buf.iter_mut() {
        *p = rng.next_u32() & 0x00FF_FFFF;
    }
}

/// 3x5 font, one byte per row, bit 2 is the left column. Letters (case folded), digits and `.,:-!?/+=`.
fn glyph(c: char) -> [u8; 5] {
    match c.to_ascii_uppercase() {
        'A' => [2, 5, 7, 5, 5],
        'B' => [6, 5, 6, 5, 6],
        'C' => [3, 4, 4, 4, 3],
        'D' => [6, 5, 5, 5, 6],
        'E' => [7, 4, 6, 4, 7],
        'F' => [7, 4, 6, 4, 4],
        'G' => [3, 4, 5, 5, 3],
        'H' => [5, 5, 7, 5, 5],
        'I' => [7, 2, 2, 2, 7],
        'J' => [1, 1, 1, 5, 2],
        'K' => [5, 5, 6, 5, 5],
        'L' => [4, 4, 4, 4, 7],
        'M' => [5, 7, 7, 5, 5],
        'N' => [6, 5, 5, 5, 5],
        'O' => [2, 5, 5, 5, 2],
        'P' => [6, 5, 6, 4, 4],
        'Q' => [2, 5, 5, 7, 3],
        'R' => [6, 5, 6, 5, 5],
        'S' => [3, 4, 2, 1, 6],
        'T' => [7, 2, 2, 2, 2],
        'U' => [5, 5, 5, 5, 7],
        'V' => [5, 5, 5, 5, 2],
        'W' => [5, 5, 7, 7, 5],
        'X' => [5, 5, 2, 5, 5],
        'Y' => [5, 5, 2, 2, 2],
        'Z' => [7, 1, 2, 4, 7],
        '0' => [7, 5, 5, 5, 7],
        '1' => [2, 6, 2, 2, 7],
        '2' => [6, 1, 2, 4, 7],
        '3' => [6, 1, 2, 1, 6],
        '4' => [5, 5, 7, 1, 1],
        '5' => [7, 4, 6, 1, 6],
        '6' => [3, 4, 7, 5, 7],
        '7' => [7, 1, 2, 2, 2],
        '8' => [7, 5, 7, 5, 7],
        '9' => [7, 5, 7, 1, 6],
        '.' => [0, 0, 0, 0, 2],
        ',' => [0, 0, 0, 2, 4],
        ':' => [0, 2, 0, 2, 0],
        '-' => [0, 0, 7, 0, 0],
        '!' => [2, 2, 2, 0, 2],
        '?' => [6, 1, 2, 0, 2],
        '/' => [1, 1, 2, 4, 4],
        '+' => [0, 2, 7, 2, 0],
        '=' => [0, 7, 0, 7, 0],
        _ => [0; 5],
    }
}

/// Draws `s` with the 3x5 font at integer `scale`; each character takes 4 * scale columns.
#[allow(clippy::too_many_arguments)] // buffer, its size, a position, a string, a scale and a colour
pub fn draw_string(buf: &mut [u32], w: usize, h: usize, x: i32, y: i32, s: &str, scale: i32, color: u32) {
    for (i, ch) in s.chars().enumerate() {
        let gx = x + i as i32 * 4 * scale;
        for (row, bits) in glyph(ch).iter().enumerate() {
            for col in 0..3 {
                if bits & (4 >> col) != 0 {
                    fill(buf, w, h, gx + col * scale, y + row as i32 * scale, scale, scale, color);
                }
            }
        }
    }
}

/// Small-text legibility test: pangram at 1x and 2x, an inverted block, and the native size.
/// Starts 8 rows down so that the frame counter (top-left) stays readable.
pub fn draw_text(buf: &mut [u32], w: usize, h: usize) {
    fill(buf, w, h, 0, 0, w as i32, h as i32, rgb(0, 0, 0));
    let white = rgb(255, 255, 255);
    let lines = [
        "THE QUICK BROWN FOX JUMPS OVER",
        "THE LAZY DOG. 0123456789",
        "ABCDEFGHIJKLMNOPQRSTUVWXYZ",
        "+-=/:.,!? PHOXIUM TEXT TEST",
    ];
    let mut y = 16;
    for l in lines {
        draw_string(buf, w, h, 4, y, l, 1, white);
        y += 8;
    }
    for l in &lines[..2] {
        draw_string(buf, w, h, 4, y, l, 2, white);
        y += 12;
    }
    let bh = (h as i32 - y - 4).clamp(0, 24);
    fill(buf, w, h, 0, y, w as i32, bh, white);
    draw_string(buf, w, h, 4, y + 2, "INVERTED 1X", 1, rgb(0, 0, 0));
    if bh > 16 {
        draw_string(buf, w, h, 4, y + 9, &format!("NATIVE {w}X{h}"), 1, rgb(0, 0, 0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: usize = 64;
    const H: usize = 48;
    /// Not a colour any pattern produces (high byte set).
    const SENTINEL: u32 = 0x0100_0000;

    fn blank() -> Vec<u32> {
        vec![SENTINEL; W * H]
    }

    #[test]
    fn bars_corners() {
        let mut b = blank();
        draw_bars(&mut b, W, H);
        assert_eq!(b[0], rgb(255, 255, 255)); // first bar, top
        assert_eq!(b[W - 1], rgb(0, 0, 0)); // last bar is black
        assert_eq!(b[W / 8], rgb(255, 255, 0)); // second bar starts at x = 8
        assert_eq!(b[(H - 1) * W], rgb(127, 127, 127)); // lower band at 50 %
        assert!(b.iter().all(|p| *p != SENTINEL), "every pixel written");
    }

    #[test]
    fn ramp_bands_run_zero_to_255() {
        let mut b = blank();
        draw_ramp(&mut b, W, H);
        for band in 0..4 {
            let y = band * H / 4 + 1;
            assert_eq!(b[y * W], 0, "band {band} starts black");
        }
        assert_eq!(b[W - 1], rgb(255, 255, 255));
        assert_eq!(b[(H / 4 + 1) * W + W - 1], rgb(255, 0, 0));
        assert_eq!(b[(H / 2 + 1) * W + W - 1], rgb(0, 255, 0));
        assert_eq!(b[(H - 1) * W + W - 1], rgb(0, 0, 255));
        let row: Vec<u32> = (0..W).map(|x| b[x] & 0xFF).collect();
        assert!(row.windows(2).all(|p| p[0] <= p[1]));
    }

    #[test]
    fn checker_cell_sizes() {
        let mut b = blank();
        draw_checker(&mut b, W, H);
        let (w, bl) = (rgb(255, 255, 255), rgb(0, 0, 0));
        assert_eq!((b[0], b[1], b[W]), (w, bl, bl)); // 1 px quadrant
        let hw = W / 2;
        assert_eq!((b[hw], b[hw + 1], b[hw + 2]), (w, w, bl)); // 2 px quadrant
        let hh = H / 2;
        assert_eq!((b[hh * W], b[hh * W + 3], b[hh * W + 4]), (w, w, bl)); // 4 px quadrant, one cell wide
        assert_eq!((b[hh * W + 4], b[hh * W + 8]), (bl, w));
        assert_eq!((b[hh * W + hw + 7], b[hh * W + hw + 8]), (w, bl)); // 8 px quadrant
    }

    #[test]
    fn motion_positions_follow_the_frame_number() {
        let blank = || vec![SENTINEL; 128 * 64];
        let mut b = blank();
        draw_motion(&mut b, 128, 64, 0, 60);
        assert_eq!(b[20 * 128 + 3], rgb(255, 255, 255)); // bar at x 0..4
        draw_motion(&mut b, 128, 64, 60, 60); // one second: half the width
        assert_eq!(b[20 * 128 + 67], rgb(255, 255, 255));
        assert_eq!(b[20 * 128 + 3], rgb(0, 0, 0));
        // Rate independence: frame 120 at 120 fps is the same picture as frame 60 at 60 fps.
        let mut c = blank();
        draw_motion(&mut c, 128, 64, 120, 120);
        assert_eq!(b, c);
        // Two consecutive frames differ.
        let mut d = blank();
        draw_motion(&mut d, 128, 64, 61, 60);
        assert_ne!(b, d);
    }

    #[test]
    fn grid_has_white_edges_and_lines() {
        let mut b = blank();
        draw_grid(&mut b, W, H);
        let white = rgb(255, 255, 255);
        for x in 0..W {
            assert_eq!(b[x], white);
            assert_eq!(b[(H - 1) * W + x], white);
        }
        for y in 0..H {
            assert_eq!(b[y * W], white);
            assert_eq!(b[y * W + W - 1], white);
        }
        assert_eq!(b[5 * W + 32], white); // line every 32
        assert_eq!(b[5 * W + 8], rgb(64, 64, 64)); // dim line every 8
        assert_eq!(b[5 * W + 5], 0);
        assert!(b.iter().all(|p| *p != SENTINEL));
    }

    #[test]
    fn markers_sit_in_the_four_corners() {
        let mut b = vec![0u32; 256 * 240];
        draw_markers(&mut b, 256, 240);
        assert_eq!(marker_size(256, 240), 15);
        let px = |x: usize, y: usize| b[y * 256 + x];
        assert_eq!(px(0, 0), MARKER_COLORS[0]);
        assert_eq!(px(14, 14), MARKER_COLORS[0]);
        assert_eq!(px(15, 15), 0);
        assert_eq!(px(255, 0), MARKER_COLORS[1]);
        assert_eq!(px(0, 239), MARKER_COLORS[2]);
        assert_eq!(px(255, 239), MARKER_COLORS[3]);
        assert_eq!(px(241, 225), MARKER_COLORS[3]);
        assert_eq!(px(240, 224), 0);
        // Tiny images still get 1 px markers, never out of bounds.
        assert_eq!(marker_size(16, 16), 1);
        let mut t = vec![0u32; 16 * 16];
        draw_markers(&mut t, 16, 16);
        assert_eq!(
            (t[0], t[15], t[240], t[255]),
            (MARKER_COLORS[0], MARKER_COLORS[1], MARKER_COLORS[2], MARKER_COLORS[3])
        );
        // Large images cap at 16.
        assert_eq!(marker_size(1920, 1080), 16);
    }

    #[test]
    fn bar_patterns_use_two_colours() {
        assert_eq!(bar_tile(BarPattern::None, 0), None);
        assert_eq!(contrast(0), rgb(64, 64, 64));
        assert_eq!(contrast(0xFFFFFF), rgb(191, 191, 191));
        assert_eq!(contrast(rgb(200, 10, 128)), rgb(136, 74, 64));
        for kind in [BarPattern::Checker, BarPattern::Stripes, BarPattern::Noise] {
            let (w, h, px) = bar_tile(kind, 0x102030).unwrap();
            assert_eq!(px.len(), w * h);
            let alt = contrast(0x102030);
            assert!(px.iter().all(|p| *p == 0x102030 || *p == alt), "{kind:?}");
            assert!(px.contains(&0x102030) && px.contains(&alt), "{kind:?}");
        }
        let (_, _, c) = bar_tile(BarPattern::Checker, 0).unwrap();
        assert_eq!((c[0], c[3], c[4], c[32], c[36]), (0, 0, contrast(0), contrast(0), 0));
        // Deterministic noise.
        assert_eq!(bar_tile(BarPattern::Noise, 0), bar_tile(BarPattern::Noise, 0));
    }

    #[test]
    fn bar_pattern_names_and_cycle() {
        for p in BarPattern::ALL {
            assert_eq!(BarPattern::parse(p.name()), Some(p));
        }
        assert_eq!(BarPattern::parse("dots"), None);
        let mut p = BarPattern::None;
        for _ in 0..BarPattern::ALL.len() {
            p = p.next();
        }
        assert_eq!(p, BarPattern::None);
    }

    #[test]
    fn noise_is_deterministic_and_seed_dependent() {
        let (mut a, mut b, mut c) = (blank(), blank(), blank());
        draw_noise(&mut a, 7);
        draw_noise(&mut b, 7);
        draw_noise(&mut c, 8);
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!(a.iter().all(|p| *p <= 0xFF_FFFF));
        let mut z = blank();
        draw_noise(&mut z, 0); // seed 0 must not degenerate
        assert!(z.iter().any(|p| *p != 0));
        assert_ne!(z[0], z[1]);
    }

    #[test]
    fn font_draws_known_glyph_pixels() {
        let mut b = vec![0u32; 16 * 16];
        // 'I' is 111 / 010 / 010 / 010 / 111 at scale 1.
        draw_string(&mut b, 16, 16, 2, 3, "I", 1, 1);
        let on = |x: usize, y: usize| b[y * 16 + x] == 1;
        assert!(on(2, 3) && on(3, 3) && on(4, 3));
        assert!(!on(2, 4) && on(3, 4) && !on(4, 4));
        assert!(on(2, 7) && on(3, 7) && on(4, 7));
        // Scale 2 doubles the glyph.
        let mut b2 = vec![0u32; 16 * 16];
        draw_string(&mut b2, 16, 16, 0, 0, "I", 2, 1);
        assert!(b2[0] == 1 && b2[1] == 1 && b2[16] == 1 && b2[17] == 1 && b2[4] == 1 && b2[5] == 1);
        // Unknown characters leave the buffer alone.
        let mut b3 = vec![0u32; 16 * 16];
        draw_string(&mut b3, 16, 16, 0, 0, "~ ", 1, 1);
        assert!(b3.iter().all(|p| *p == 0));
    }

    #[test]
    fn text_content_has_white_and_black_and_is_stable() {
        let (mut a, mut b) = (vec![5u32; 256 * 240], vec![9u32; 256 * 240]);
        draw_text(&mut a, 256, 240);
        draw_text(&mut b, 256, 240);
        assert_eq!(a, b);
        assert!(a.contains(&rgb(255, 255, 255)) && a.contains(&0));
    }
}
