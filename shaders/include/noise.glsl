// Deterministic integer noise, the same arithmetic as crates/crt-model/src/noise.rs (the oracle): a 32-bit hash of
// a seed and three counters (a pixel, a line, a frame) and the uniform and near-Gaussian numbers made of it. Only
// wrapping integer operations and exact conversions of 24-bit integers, so the shader and the oracle draw the
// same numbers.
#ifndef PHOXIUM_NOISE_GLSL
#define PHOXIUM_NOISE_GLSL

// lowbias32 (Chris Wellons).
uint noise_hash(uint x) {
    x ^= x >> 16;
    x *= 0x7feb352du;
    x ^= x >> 15;
    x *= 0x846ca68bu;
    x ^= x >> 16;
    return x;
}

uint noise_cell(uint seed, uint a, uint b, uint c) {
    return noise_hash(noise_hash(noise_hash(noise_hash(seed) ^ a) ^ b) ^ c);
}

// [0, 1) from the top 24 bits.
float noise_unit(uint h) {
    return float(h >> 8) * (1.0 / 16777216.0);
}

// [-1, 1).
float noise_symmetric(uint h) {
    return 2.0 * noise_unit(h) - 1.0;
}

// Zero mean, unit variance, bounded at +-3.46: four uniform numbers (Irwin-Hall).
float noise_gaussian(uint h) {
    float sum = 0.0;
    for (uint i = 0u; i < 4u; ++i) {
        sum += noise_unit(noise_hash(h + i * 0x9e3779b9u));
    }
    return (sum - 2.0) * 1.7320508075688772;
}

#endif
