//! Binary-coded frame counter drawn in the top-left corner of the native image, and its decoder,
//! so that a capture can tell which frame it shows (latency, cadence, dropped frames).
//!
//! Format: one row of `BITS + 2` square cells of `c x c` native pixels (`c = min(8, w / CELLS)`,
//! at least 1, at most the image height): cell 0 is a white sync cell, cells 1..=32 are the value
//! most significant bit first (white = 1, black = 0), the last cell is black. Needs a native width
//! of at least `CELLS` (34) pixels, otherwise nothing is drawn. The decoder samples the centre of
//! each cell and thresholds the mean of the channels at 128, so it survives a scale-up and mild
//! filtering.

use crate::layout::{native_to_client, Rect};

pub const BITS: usize = 32;
pub const CELLS: usize = BITS + 2;
const WHITE: u32 = 0x00FF_FFFF;
const BLACK: u32 = 0;

/// Cell size in native pixels, or `None` when the counter does not fit.
pub fn cell_size(w: usize, h: usize) -> Option<usize> {
    let c = (w / CELLS).min(8);
    if c == 0 || c > h {
        None
    } else {
        Some(c)
    }
}

/// Draws `value` into the top-left corner of a `w x h` buffer.
pub fn draw(buf: &mut [u32], w: usize, h: usize, value: u32) {
    let Some(c) = cell_size(w, h) else { return };
    for cell in 0..CELLS {
        let on = match cell {
            0 => true,
            n if n <= BITS => (value >> (BITS - n)) & 1 == 1,
            _ => false,
        };
        let color = if on { WHITE } else { BLACK };
        for y in 0..c {
            let row = y * w + cell * c;
            buf[row..row + c].fill(color);
        }
    }
}

/// Decodes a counter. `sample(x, y)` returns the 0x00RRGGBB colour at native coordinates.
pub fn decode(w: usize, h: usize, sample: impl Fn(usize, usize) -> u32) -> Option<u32> {
    let c = cell_size(w, h)?;
    let on = |cell: usize| {
        let p = sample(cell * c + c / 2, c / 2);
        let mean = (((p >> 16) & 0xFF) + ((p >> 8) & 0xFF) + (p & 0xFF)) / 3;
        mean >= 128
    };
    if !on(0) || on(CELLS - 1) {
        return None;
    }
    let mut v = 0u32;
    for cell in 1..=BITS {
        v = (v << 1) | u32::from(on(cell));
    }
    Some(v)
}

/// Decodes the counter from a captured client image `fw x fh` in which the native image of
/// `nw x nh` is displayed in `dest` (as computed by the layout).
pub fn decode_frame(frame: &[u32], fw: usize, fh: usize, dest: Rect, nw: usize, nh: usize) -> Option<u32> {
    decode(nw, nh, |x, y| {
        let (cx, cy) = native_to_client(dest, nw as u32, nh as u32, x as u32, y as u32);
        if cx < 0 || cy < 0 || cx as usize >= fw || cy as usize >= fh {
            0
        } else {
            frame[cy as usize * fw + cx as usize]
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{layout, Align, Par, ScaleMode};

    fn roundtrip(w: usize, h: usize, v: u32) -> Option<u32> {
        let mut buf = vec![0x00404040; w * h];
        draw(&mut buf, w, h, v);
        decode(w, h, |x, y| buf[y * w + x])
    }

    #[test]
    fn cell_sizes() {
        assert_eq!(cell_size(256, 240), Some(7)); // 256 / 34 = 7
        assert_eq!(cell_size(320, 240), Some(8)); // 320 / 34 = 9 -> capped at 8
        assert_eq!(cell_size(34, 240), Some(1));
        assert_eq!(cell_size(33, 240), None);
        assert_eq!(cell_size(1920, 4), None); // cell 8 taller than the image
    }

    #[test]
    fn round_trip_edge_values() {
        for v in [0, 1, 2, 255, 256, 0x8000_0000, 0xDEAD_BEEF, u32::MAX, 123_456_789] {
            assert_eq!(roundtrip(256, 240, v), Some(v), "{v:#x}");
            assert_eq!(roundtrip(34, 16, v), Some(v), "{v:#x} at the smallest width");
        }
    }

    #[test]
    fn round_trip_consecutive_frames() {
        for v in 0..2000u32 {
            assert_eq!(roundtrip(320, 240, v), Some(v));
        }
    }

    #[test]
    fn too_narrow_draws_nothing_and_decodes_none() {
        let mut buf = vec![0x00123456; 33 * 20];
        draw(&mut buf, 33, 20, 7);
        assert!(buf.iter().all(|p| *p == 0x00123456));
        assert_eq!(decode(33, 20, |_, _| 0), None);
    }

    #[test]
    fn bad_sync_is_rejected() {
        assert_eq!(decode(256, 240, |_, _| 0), None); // all black: no sync
        assert_eq!(decode(256, 240, |_, _| WHITE), None); // all white: end cell is not black
    }

    #[test]
    fn decode_survives_a_scaled_capture_with_bars() {
        let (nw, nh) = (256usize, 240usize);
        let mut native = vec![0u32; nw * nh];
        draw(&mut native, nw, nh, 0x0123_4567);
        // 1920x1080 client, integer scale 4 centred: dest 1024x960 at (448, 60).
        let lay = layout(
            1920,
            1080,
            nw as u32,
            nh as u32,
            Par::SQUARE,
            ScaleMode::Fit,
            Align::Center,
        );
        let (fw, fh) = (1920usize, 1080usize);
        let mut frame = vec![0u32; fw * fh];
        for y in 0..fh {
            for x in 0..fw {
                let (dx, dy) = (x as i32 - lay.dest.x, y as i32 - lay.dest.y);
                if dx >= 0 && dy >= 0 && dx < lay.dest.w && dy < lay.dest.h {
                    frame[y * fw + x] = native[(dy as usize / 4) * nw + dx as usize / 4];
                }
            }
        }
        assert_eq!(decode_frame(&frame, fw, fh, lay.dest, nw, nh), Some(0x0123_4567));
    }

    #[test]
    fn decode_survives_a_stretched_capture_with_markers() {
        // Non-integer, aspect-breaking stretch (nearest), markers drawn before the counter.
        let (nw, nh) = (256usize, 240usize);
        let mut native = vec![0u32; nw * nh];
        crate::patterns::draw_markers(&mut native, nw, nh);
        draw(&mut native, nw, nh, 0x00AB_CDEF);
        let (fw, fh) = (1001usize, 333usize);
        let lay = layout(
            fw as u32,
            fh as u32,
            nw as u32,
            nh as u32,
            Par::SQUARE,
            ScaleMode::Stretch,
            Align::Center,
        );
        assert_eq!(lay.dest, Rect::new(0, 0, fw as i32, fh as i32));
        let frame: Vec<u32> = (0..fw * fh)
            .map(|i| {
                let (x, y) = (i % fw, i / fw);
                native[(y * nh / fh) * nw + x * nw / fw]
            })
            .collect();
        assert_eq!(decode_frame(&frame, fw, fh, lay.dest, nw, nh), Some(0x00AB_CDEF));
        // The bottom-right capture pixel is the yellow marker.
        assert_eq!(frame[fw * fh - 1], crate::patterns::MARKER_COLORS[3]);
    }
}
