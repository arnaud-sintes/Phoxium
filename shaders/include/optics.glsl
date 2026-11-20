// Slot `optics` (T-45; CRT.md rule 7 and "Glass, halation, glare and reflections"; QUESTIONS PHY-10): shared
// declarations of the stage's passes (src/gpu/stages/optics.rs). Model and derivation: the oracle
// crates/crt-model/src/optics.rs. All passes use the standard set 0 (Params, FrameConstants, the chain input
// at binding 2, the chain output at binding 3) and the stage's set 1 below, with one push-constant block.
//
// Set 1 (each pass has its own descriptor set; every binding is written, unused ones with a 1 x 1 image):
//   0 OPT_SRC   pass input (down: level k - 1; kernel: level l; gauss_v: per-Gaussian red sums)
//   1 OPT_DST   pass output (down: level k; kernel: the halo at level l; gauss_h: red sums)
//   2, 3        gauss_h outputs / gauss_v inputs (green, blue sums)
//   4 OPT_HALO  the halo (kernel: at level l; gaussians: full resolution), read by the composite
//   5 OPT_TOP   the 1 x 1 top of the pyramid (box mean of the whole image, zero padded)
//   6 to 17     pyramid levels 1 to 12 (pyramid method)
//   18          storage buffer: diffusion weights w(d), d = 0 .. OPT_DIFF_MAX, then the halo kernel quadrant
//               (R + 1)^2, index dy (R + 1) + dx for |dx|, |dy| <= R
#ifndef UCRT_OPTICS_GLSL
#define UCRT_OPTICS_GLSL

#define OPT_DIFF_MAX 16
#define OPT_KERNEL_OFFSET (OPT_DIFF_MAX + 1)

// std430, 96 bytes, mirrored by `Push` in src/gpu/stages/optics.rs. The meaning of each field is per pass.
layout(push_constant) uniform OpticsPush {
    ivec4 info;
    vec4 scal;
    vec4 w[4];
} pc;

layout(std430, set = 1, binding = 18) readonly buffer OpticsWeights {
    float weight[];
} opt_buf;

// Bilinear read of a level image IM (texel = mean of a 2^lvl x 2^lvl block of host pixels) at the host pixel
// p: the texel centre of block c is host coordinate c 2^lvl + (2^lvl - 1) / 2. Clamped to the edge texels.
#define OPT_BILINEAR(NAME, IM)                                                                                 \
    vec3 NAME(ivec2 p, int lvl) {                                                                              \
        float s = float(1 << lvl);                                                                             \
        vec2 u = (vec2(p) - 0.5 * (s - 1.0)) / s;                                                              \
        ivec2 sz = imageSize(IM);                                                                              \
        vec2 fl = floor(u);                                                                                    \
        vec2 f = u - fl;                                                                                       \
        ivec2 i0 = ivec2(fl);                                                                                  \
        ivec2 a = clamp(i0, ivec2(0), sz - 1);                                                                 \
        ivec2 b = clamp(i0 + 1, ivec2(0), sz - 1);                                                             \
        vec3 v00 = imageLoad(IM, a).rgb;                                                                       \
        vec3 v10 = imageLoad(IM, ivec2(b.x, a.y)).rgb;                                                         \
        vec3 v01 = imageLoad(IM, ivec2(a.x, b.y)).rgb;                                                         \
        vec3 v11 = imageLoad(IM, b).rgb;                                                                       \
        return mix(mix(v00, v10, f.x), mix(v01, v11, f.x), f.y);                                              \
    }

#endif
