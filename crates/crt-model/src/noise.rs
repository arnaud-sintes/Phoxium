//! Deterministic integer noise shared by the oracle and the shaders (`shaders/include/noise.glsl` is the same
//! arithmetic): a 32-bit hash of a seed and three counters (a pixel, a line, a frame), and the uniform and
//! near-Gaussian numbers made of it. Everything is integer arithmetic with wrap-around plus exact conversions
//! of 24-bit integers to floats, so the CPU and the GPU draw the same numbers.
//!
//! Used by the composite decoder's burst phase jitter (`composite_hue_jitter_deg`) and the analogue
//! impairments (`crt_model::impair`).

/// The 32-bit finaliser "lowbias32" (Chris Wellons): a bijective mix, every input bit affects every output bit.
pub fn hash(mut x: u32) -> u32 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb_352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846c_a68b);
    x ^= x >> 16;
    x
}

/// The hash of a seed (which noise this is) and three counters.
pub fn cell(seed: u32, a: u32, b: u32, c: u32) -> u32 {
    hash(hash(hash(hash(seed) ^ a) ^ b) ^ c)
}

/// A uniform number in [0, 1) from the top 24 bits of `h` (exact in f32 and f64).
pub fn unit(h: u32) -> f64 {
    f64::from(h >> 8) / 16_777_216.0
}

/// A uniform number in [-1, 1) from `h`.
pub fn symmetric(h: u32) -> f64 {
    2.0 * unit(h) - 1.0
}

/// A near-Gaussian number of zero mean and unit variance from `h`: four uniform numbers (the hash of `h` and
/// of three more mixes of it) summed, centred and scaled (Irwin-Hall, bounded at +-3.46). Plain arithmetic, so
/// the shader and the oracle agree to the last bit.
pub fn gaussian(h: u32) -> f64 {
    let sum: f64 = (0..4u32)
        .map(|i| unit(hash(h.wrapping_add(i.wrapping_mul(0x9e37_79b9)))))
        .sum();
    (sum - 2.0) * 3.0_f64.sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hash is a fixed function (known values pin the constants the shader repeats) and spreads its input.
    #[test]
    fn the_hash_is_pinned_and_uniform() {
        assert_eq!(hash(0), 0);
        assert_eq!(hash(1), 0x6889_90c0);
        assert_eq!(hash(0xffff_ffff), 0x6768_824a);
        let n = 200_000u32;
        let mean = (0..n).map(|i| unit(cell(7, i, 3, 1))).sum::<f64>() / f64::from(n);
        assert!((mean - 0.5).abs() < 3e-3, "{mean}");
        assert!((0..1000).all(|i| (0.0..1.0).contains(&unit(cell(1, i, 0, 0)))));
    }

    /// The near-Gaussian number has mean 0 and variance 1 over many cells, and a counter changes the draw.
    #[test]
    fn the_gaussian_has_unit_variance() {
        let n = 200_000u32;
        let v: Vec<f64> = (0..n).map(|i| gaussian(cell(9, i, 0, 0))).collect();
        let mean = v.iter().sum::<f64>() / f64::from(n);
        let var = v.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / f64::from(n);
        assert!(mean.abs() < 1e-2, "{mean}");
        assert!((var - 1.0).abs() < 2e-2, "{var}");
        assert!(v.iter().all(|x| x.abs() <= 3.47));
        assert_ne!(cell(1, 2, 3, 4), cell(1, 2, 3, 5));
        assert_ne!(cell(1, 2, 3, 4), cell(2, 2, 3, 4));
    }
}
