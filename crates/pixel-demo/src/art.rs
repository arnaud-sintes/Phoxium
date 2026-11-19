//! Procedural pixel art for the demo source: a NES-style scene and a test card.
//! Pixels are 0x00RRGGBB, written into a `w * h` buffer.

pub struct Image {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u32>,
}

pub const fn rgb(r: u8, g: u8, b: u8) -> u32 {
    ((r as u32) << 16) | ((g as u32) << 8) | b as u32
}

/// Fills a rectangle, clipped to the buffer.
#[allow(clippy::too_many_arguments)] // buffer, its size, a rectangle and a colour: a struct would only add noise
pub(crate) fn fill(buf: &mut [u32], w: usize, h: usize, x0: i32, y0: i32, rw: i32, rh: i32, color: u32) {
    let xa = x0.max(0);
    let ya = y0.max(0);
    let xb = (x0 + rw).min(w as i32);
    let yb = (y0 + rh).min(h as i32);
    for y in ya..yb {
        let row = y as usize * w;
        for x in xa..xb {
            buf[row + x as usize] = color;
        }
    }
}

fn put(buf: &mut [u32], w: usize, h: usize, x: i32, y: i32, color: u32) {
    if x >= 0 && y >= 0 && (x as usize) < w && (y as usize) < h {
        buf[y as usize * w + x as usize] = color;
    }
}

const BAYER4: [[usize; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];

const HERO: [&str; 16] = [
    "................",
    "....kkkkkk......",
    "...krrrrrrk.....",
    "..krrrrrrrrk....",
    "..kkkssskssk....",
    ".kssksssssssk...",
    ".kssksssksssk...",
    ".kksssssskkk....",
    "..kssssssk......",
    "...kkbbbkk......",
    "..kbbbbbbbk.....",
    ".kbbwbbbbwbbk...",
    ".kbbbbbbbbbbk...",
    "..kkbbkkbbkk....",
    "..kbbk..kbbk....",
    "..kkk....kkk....",
];

const HEART: [&str; 6] = [".kk.kk.", "krrkrrk", "krrrrrk", ".krrrk.", "..krk..", "...k..."];

fn sprite_color(c: u8) -> Option<u32> {
    match c {
        b'k' => Some(rgb(0, 0, 0)),
        b'r' => Some(rgb(216, 40, 0)),
        b's' => Some(rgb(252, 188, 176)),
        b'b' => Some(rgb(0, 88, 248)),
        b'w' => Some(rgb(252, 252, 252)),
        _ => None,
    }
}

fn draw_sprite(buf: &mut [u32], w: usize, h: usize, rows: &[&str], x: i32, y: i32, flip: bool) {
    for (ry, row) in rows.iter().enumerate() {
        let bytes = row.as_bytes();
        for rx in 0..bytes.len() {
            if let Some(color) = sprite_color(bytes[rx]) {
                let dx = if flip { bytes.len() - 1 - rx } else { rx };
                put(buf, w, h, x + dx as i32, y + ry as i32, color);
            }
        }
    }
}

/// Animated side-scrolling scene: dithered sky, clouds, two hill layers, bricks, a hopping hero
/// and a static HUD (useful later for burn-in tests).
pub fn draw_scene(buf: &mut [u32], w: usize, h: usize, t: u32) {
    let ground_y = (h - (h / 5).max(16)) as i32;
    let sky = [
        rgb(60, 120, 230),
        rgb(92, 148, 252),
        rgb(140, 190, 252),
        rgb(188, 216, 252),
    ];

    // Sky with 4x4 ordered dither between the four bands.
    for y in 0..ground_y as usize {
        let level = y * 3 * 16 / ground_y as usize;
        let band = level / 16;
        let rem = level % 16;
        for x in 0..w {
            let idx = (band + usize::from(rem > BAYER4[y & 3][x & 3])).min(3);
            buf[y * w + x] = sky[idx];
        }
    }
    for y in ground_y as usize..h {
        for x in 0..w {
            buf[y * w + x] = rgb(0, 0, 0);
        }
    }

    // Clouds drifting left.
    let span = w as i32 + 64;
    for (i, cy) in [(0i32, 14i32), (1, 34), (2, 22)] {
        let cx = (i * 97 + 40 - (t / 3) as i32).rem_euclid(span) - 32;
        for (dx, dy, rw, rh) in [(4, 4, 24, 8), (0, 8, 32, 8), (8, 0, 16, 8)] {
            fill(buf, w, h, cx + dx, cy + dy, rw, rh, rgb(252, 252, 252));
        }
        fill(buf, w, h, cx, cy + 14, 32, 2, rgb(188, 188, 188));
    }

    // Two hill layers with different scroll speeds.
    for x in 0..w as i32 {
        let far = 18 + (10.0 * ((x as f32 + t as f32 / 6.0) * 0.03).sin()) as i32;
        fill(buf, w, h, x, ground_y - far, 1, far, rgb(0, 128, 60));
        let near = 8 + (6.0 * ((x as f32 + t as f32 / 2.0) * 0.07 + 2.0).sin()) as i32;
        fill(buf, w, h, x, ground_y - near, 1, near, rgb(0, 168, 0));
    }

    // Scrolling bricks.
    let scroll = t as i32;
    for y in ground_y..h as i32 {
        let ry = y - ground_y;
        let row = ry / 8;
        let offset = if row % 2 == 1 { 8 } else { 0 };
        for x in 0..w as i32 {
            let sx = x + scroll + offset;
            let mortar = ry % 8 == 0 || sx.rem_euclid(16) == 0;
            let color = if mortar {
                rgb(0, 0, 0)
            } else if ry % 8 == 1 {
                rgb(252, 152, 56)
            } else {
                rgb(200, 76, 12)
            };
            buf[y as usize * w + x as usize] = color;
        }
    }

    // Hero walking back and forth, hopping once per second.
    let period = 480u32;
    let tt = t % period;
    let (pos, flip) = if tt < period / 2 {
        (tt, false)
    } else {
        (period - tt, true)
    };
    let range = (w as i32 - 32).max(1);
    let hx = 16 + range * pos as i32 / (period / 2) as i32;
    let hop = (((t % 60) as f32 / 60.0 * std::f32::consts::PI).sin() * 14.0) as i32;
    draw_sprite(buf, w, h, &HERO, hx, ground_y - 16 - hop, flip);

    // Static HUD.
    for i in 0..3 {
        draw_sprite(buf, w, h, &HEART, 8 + i * 9, 8, false);
    }
}

/// Static and moving test card: colour bars, grey ramp, 1-pixel line patterns, checkerboards,
/// windows, a crosshatch, a moving line and square, and corner markers for crop alignment.
pub fn draw_card(buf: &mut [u32], w: usize, h: usize, t: u32) {
    let (wi, hi) = (w as i32, h as i32);
    fill(buf, w, h, 0, 0, wi, hi, rgb(16, 16, 16));
    let band = (hi / 8).max(8);

    // Band 0: colour bars.
    let bars = [
        rgb(255, 255, 255),
        rgb(255, 255, 0),
        rgb(0, 255, 255),
        rgb(0, 255, 0),
        rgb(255, 0, 255),
        rgb(255, 0, 0),
        rgb(0, 0, 255),
        rgb(0, 0, 0),
    ];
    for (i, c) in bars.iter().enumerate() {
        let x0 = wi * i as i32 / 8;
        let x1 = wi * (i as i32 + 1) / 8;
        fill(buf, w, h, x0, 0, x1 - x0, band, *c);
    }

    // Band 1: 16-step grey ramp.
    for i in 0..16 {
        let v = (i * 17) as u8;
        let x0 = wi * i / 16;
        let x1 = wi * (i + 1) / 16;
        fill(buf, w, h, x0, band, x1 - x0, band, rgb(v, v, v));
    }

    // Band 2-3: 1-pixel line patterns on white, four quarters:
    // alternating columns, isolated vertical lines, isolated horizontal lines, alternating rows.
    let white = rgb(255, 255, 255);
    let black = rgb(0, 0, 0);
    let q = wi / 4;
    fill(buf, w, h, 0, 2 * band, wi, 2 * band, white);
    for y in 2 * band..4 * band {
        for x in 0..wi {
            let quarter = (x / q.max(1)).min(3);
            let lx = x - quarter * q;
            let ly = y - 2 * band;
            let on = match quarter {
                0 => lx % 2 == 0,
                1 => lx % 8 == 4,
                2 => ly % 8 == 4,
                _ => ly % 2 == 0,
            };
            if on {
                put(buf, w, h, x, y, black);
            }
        }
    }

    // Band 4-5: 1px checker, 2px checker, three white windows, crosshatch.
    let y0 = 4 * band;
    for y in y0..6 * band {
        for x in 0..wi {
            let quarter = (x / q.max(1)).min(3);
            let lx = x - quarter * q;
            let ly = y - y0;
            let c = match quarter {
                0 => (lx + ly) % 2 == 0,
                1 => ((lx / 2) + (ly / 2)) % 2 == 0,
                2 => false,
                _ => lx % 8 == 0 || ly % 8 == 0,
            };
            let color = match quarter {
                3 => {
                    if c {
                        rgb(96, 96, 96)
                    } else {
                        rgb(16, 16, 16)
                    }
                }
                _ => {
                    if c {
                        white
                    } else {
                        black
                    }
                }
            };
            put(buf, w, h, x, y, color);
        }
    }
    let wx = 2 * q + 4;
    for (i, side) in [6, 12, 20].iter().enumerate() {
        fill(buf, w, h, wx + i as i32 * 28, y0 + 4, *side, *side, white);
    }

    // Band 6-7: motion area with a moving 1px vertical line, a moving square and a moving 1px
    // horizontal line.
    let my = 6 * band;
    fill(buf, w, h, 0, my, wi, hi - my, rgb(0, 0, 0));
    put_vline(buf, w, h, (t as i32 * 2) % wi, my, hi - my, white);
    fill(buf, w, h, (t as i32 * 4) % wi, my + (hi - my) / 3, 8, 8, white);
    let hy = my + (t as i32 / 2) % (hi - my).max(1);
    fill(buf, w, h, 0, hy, wi / 3, 1, white);

    // 1px white frame and colour corner markers (crop alignment check).
    fill(buf, w, h, 0, 0, wi, 1, white);
    fill(buf, w, h, 0, hi - 1, wi, 1, white);
    fill(buf, w, h, 0, 0, 1, hi, white);
    fill(buf, w, h, wi - 1, 0, 1, hi, white);
    fill(buf, w, h, 1, 1, 4, 4, rgb(255, 0, 0));
    fill(buf, w, h, wi - 5, 1, 4, 4, rgb(0, 255, 0));
    fill(buf, w, h, 1, hi - 5, 4, 4, rgb(0, 0, 255));
    fill(buf, w, h, wi - 5, hi - 5, 4, 4, rgb(255, 255, 0));
}

fn put_vline(buf: &mut [u32], w: usize, h: usize, x: i32, y: i32, len: i32, color: u32) {
    fill(buf, w, h, x, y, 1, len, color);
}

/// Shows a loaded image centred, on black.
pub fn draw_image(buf: &mut [u32], w: usize, h: usize, img: &Image) {
    buf.fill(0);
    let ox = (w as i32 - img.w as i32) / 2;
    let oy = (h as i32 - img.h as i32) / 2;
    for y in 0..img.h {
        for x in 0..img.w {
            put(buf, w, h, ox + x as i32, oy + y as i32, img.px[y * img.w + x]);
        }
    }
}

/// Loads a PNG (any bit depth, indexed or not) into 0x00RRGGBB pixels. Alpha is composited on black.
pub fn load_png(path: &str) -> Result<Image, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{path}: {e}"))?;
    let mut decoder = png::Decoder::new(std::io::BufReader::new(file));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(|e| format!("{path}: {e}"))?;
    let mut data = vec![0u8; reader.output_buffer_size().ok_or("image too large")?];
    let info = reader.next_frame(&mut data).map_err(|e| format!("{path}: {e}"))?;
    let (w, h) = (info.width as usize, info.height as usize);
    let channels = match info.color_type {
        png::ColorType::Grayscale => 1,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        png::ColorType::Indexed => return Err("indexed PNG was not expanded".into()),
    };
    let mut px = Vec::with_capacity(w * h);
    for i in 0..w * h {
        let p = &data[i * channels..(i + 1) * channels];
        let (r, g, b, a) = match channels {
            1 => (p[0], p[0], p[0], 255),
            2 => (p[0], p[0], p[0], p[1]),
            3 => (p[0], p[1], p[2], 255),
            _ => (p[0], p[1], p[2], p[3]),
        };
        let blend = |c: u8| ((c as u32 * a as u32) / 255) as u8;
        px.push(rgb(blend(r), blend(g), blend(b)));
    }
    Ok(Image { w, h, px })
}
