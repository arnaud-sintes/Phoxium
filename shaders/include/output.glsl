// Output transform helpers of the `encode` stage (T-46): soft shoulder, dither thresholds. The f64 oracle
// is crates/crt-model/src/output.rs; keep both in step.
#ifndef UCRT_OUTPUT_GLSL
#define UCRT_OUTPUT_GLSL

// A channel counts as clipped above 1 + CLIP_EPS or below -CLIP_EPS: float noise of the colour matrix
// (rows sum to 1 within 1e-7) must not turn tube white into a clipped pixel.
const float CLIP_EPS = 1e-4;

// Soft shoulder of one linear channel: 0 for negative values; the value up to `knee`; above it
// k + (1 - k) (1 - exp(-(x - k) / (1 - k))) (slope 1 at the knee, monotonic, asymptote 1). knee >= 1 is a
// hard clip at 1. [E]
float shoulder(float x, float knee) {
    if (x <= 0.0) {
        return 0.0;
    }
    if (knee >= 1.0) {
        return min(x, 1.0);
    }
    if (x <= knee) {
        return x;
    }
    return knee + (1.0 - knee) * (1.0 - exp(-(x - knee) / (1.0 - knee)));
}

vec3 shoulder(vec3 x, float knee) {
    return vec3(shoulder(x.r, knee), shoulder(x.g, knee), shoulder(x.b, knee));
}

// Entry (x, y) of the 8x8 Bayer matrix, 0..63: bits a0 y0 a1 y1 a2 y2 from the most significant bit, with
// a = x ^ y (row 0: 0 32 8 40 2 34 10 42).
uint bayer8(ivec2 p) {
    uint x = uint(p.x) & 7u;
    uint y = uint(p.y) & 7u;
    uint a = x ^ y;
    return ((a & 1u) << 5) | ((y & 1u) << 4) | (((a >> 1) & 1u) << 3) | (((y >> 1) & 1u) << 2)
        | (((a >> 2) & 1u) << 1) | ((y >> 2) & 1u);
}

// Ordered dither threshold in [0, 1): (bayer8 + 0.5) / 64.
float ordered_threshold(ivec2 p) {
    return (float(bayer8(p)) + 0.5) / 64.0;
}

// Noise dither threshold in [0, 1): interleaved gradient noise (Jimenez 2014), whose spectrum is
// concentrated at high spatial frequencies (blue-noise-like), shifted by 5.588238 px per frame (the
// frame counter wraps at 64 to keep the argument small). Deterministic for a given pixel and frame.
float noise_threshold(vec2 f, uint frame_index) {
    vec2 p = floor(f) + 5.588238 * float(frame_index & 63u);
    return fract(52.9829189 * fract(dot(p, vec2(0.06711056, 0.00583715))));
}

#endif
