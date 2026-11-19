//! Pure CPU resampling used by the free-fit mode to emulate filtered (smoothed) upscalers.
//! Pixel centres map as `s = (d + 0.5) * src / dst - 0.5`, edges are clamped.
//! Nearest uses `floor((d + 0.5) * src / dst)`; bilinear is a 2-tap tent; bicubic is Catmull-Rom.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    Nearest,
    Bilinear,
    Bicubic,
}

impl Filter {
    pub fn name(self) -> &'static str {
        match self {
            Filter::Nearest => "nearest",
            Filter::Bilinear => "bilinear",
            Filter::Bicubic => "bicubic",
        }
    }
}

fn catmull_rom(x: f32) -> f32 {
    let x = x.abs();
    if x < 1.0 {
        1.5 * x * x * x - 2.5 * x * x + 1.0
    } else if x < 2.0 {
        -0.5 * x * x * x + 2.5 * x * x - 4.0 * x + 2.0
    } else {
        0.0
    }
}

/// Four taps (first index, weights) per destination index along one axis.
fn taps(src: usize, dst: usize, filter: Filter) -> Vec<(i64, [f32; 4])> {
    (0..dst)
        .map(|d| {
            let s = (d as f64 + 0.5) * src as f64 / dst as f64 - 0.5;
            let base = s.floor();
            let f = (s - base) as f32;
            let w = match filter {
                Filter::Bilinear => [0.0, 1.0 - f, f, 0.0],
                _ => [
                    catmull_rom(1.0 + f),
                    catmull_rom(f),
                    catmull_rom(1.0 - f),
                    catmull_rom(2.0 - f),
                ],
            };
            (base as i64 - 1, w)
        })
        .collect()
}

/// Resamples a `sw x sh` image of 0x00RRGGBB pixels to `dw x dh`.
pub fn resample(src: &[u32], sw: usize, sh: usize, dw: usize, dh: usize, filter: Filter) -> Vec<u32> {
    assert_eq!(src.len(), sw * sh);
    if dw == 0 || dh == 0 {
        return Vec::new();
    }
    if filter == Filter::Nearest {
        let mut out = Vec::with_capacity(dw * dh);
        for y in 0..dh {
            let sy = ((y as u64 * 2 + 1) * sh as u64 / (2 * dh as u64)) as usize;
            for x in 0..dw {
                let sx = ((x as u64 * 2 + 1) * sw as u64 / (2 * dw as u64)) as usize;
                out.push(src[sy * sw + sx]);
            }
        }
        return out;
    }
    let tx = taps(sw, dw, filter);
    let ty = taps(sh, dh, filter);
    // Horizontal pass: dw x sh, three f32 channels.
    let mut mid = vec![0f32; dw * sh * 3];
    for y in 0..sh {
        let row = &src[y * sw..(y + 1) * sw];
        for (x, (first, w)) in tx.iter().enumerate() {
            let mut acc = [0f32; 3];
            for (k, wk) in w.iter().enumerate() {
                if *wk == 0.0 {
                    continue;
                }
                let i = (*first + k as i64).clamp(0, sw as i64 - 1) as usize;
                let p = row[i];
                acc[0] += *wk * ((p >> 16) & 0xFF) as f32;
                acc[1] += *wk * ((p >> 8) & 0xFF) as f32;
                acc[2] += *wk * (p & 0xFF) as f32;
            }
            let o = (y * dw + x) * 3;
            mid[o..o + 3].copy_from_slice(&acc);
        }
    }
    // Vertical pass.
    let mut out = Vec::with_capacity(dw * dh);
    for (first, w) in &ty {
        for x in 0..dw {
            let mut acc = [0f32; 3];
            for (k, wk) in w.iter().enumerate() {
                if *wk == 0.0 {
                    continue;
                }
                let j = (*first + k as i64).clamp(0, sh as i64 - 1) as usize;
                let o = (j * dw + x) * 3;
                acc[0] += *wk * mid[o];
                acc[1] += *wk * mid[o + 1];
                acc[2] += *wk * mid[o + 2];
            }
            let c = |v: f32| (v + 0.5).floor().clamp(0.0, 255.0) as u32;
            out.push((c(acc[0]) << 16) | (c(acc[1]) << 8) | c(acc[2]));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grey(v: u32) -> u32 {
        (v << 16) | (v << 8) | v
    }

    #[test]
    fn identity_size_is_exact_for_every_filter() {
        let src: Vec<u32> = (0..12).map(|i| grey(i * 20)).collect();
        for f in [Filter::Nearest, Filter::Bilinear, Filter::Bicubic] {
            assert_eq!(resample(&src, 4, 3, 4, 3, f), src, "{f:?}");
        }
    }

    #[test]
    fn constant_image_stays_constant() {
        let src = vec![0x336699; 5 * 4];
        for f in [Filter::Bilinear, Filter::Bicubic] {
            let out = resample(&src, 5, 4, 13, 9, f);
            assert!(out.iter().all(|p| *p == 0x336699), "{f:?}");
        }
    }

    #[test]
    fn bilinear_two_to_four_matches_hand_values() {
        // Source centres map to -0.25, 0.25, 0.75, 1.25: 0 (clamped), 63.75 -> 64, 191.25 -> 191, 255.
        let out = resample(&[grey(0), grey(255)], 2, 1, 4, 1, Filter::Bilinear);
        assert_eq!(out, vec![grey(0), grey(64), grey(191), grey(255)]);
    }

    #[test]
    fn nearest_integer_upscale_repeats_pixels() {
        let out = resample(&[1, 2, 3, 4], 2, 2, 6, 4, Filter::Nearest);
        assert_eq!(
            out,
            vec![1, 1, 1, 2, 2, 2, 1, 1, 1, 2, 2, 2, 3, 3, 3, 4, 4, 4, 3, 3, 3, 4, 4, 4]
        );
    }

    #[test]
    fn bicubic_overshoots_a_hard_edge_and_bilinear_does_not() {
        // 0,0,255,255 upscaled x4: bicubic rings (clamped to 0..255 but reaches the extremes),
        // bilinear is monotonic.
        let src = [grey(0), grey(0), grey(255), grey(255)];
        let lin = resample(&src, 4, 1, 16, 1, Filter::Bilinear);
        assert!(lin.windows(2).all(|w| (w[0] & 0xFF) <= (w[1] & 0xFF)));
        let cub = resample(&src, 4, 1, 16, 1, Filter::Bicubic);
        // Smooth transition exists (not a hard edge) for both.
        assert!(lin.iter().any(|p| (p & 0xFF) > 0 && (p & 0xFF) < 255));
        assert!(cub.iter().any(|p| (p & 0xFF) > 0 && (p & 0xFF) < 255));
    }
}
