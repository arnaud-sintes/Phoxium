#version 450
#extension GL_GOOGLE_include_directive : require

// Stage `encode` (ARCHITECTURE 3.7, T-46): linear tube light -> output target (swapchain or headless
// image).
//
// Inside the tube rectangle, for the linear value L of the tube-domain texel under the fragment (the
// last chain image, one texel per host pixel of the tube rectangle, T-20b):
//   1. exposure: E = exposure * L (tube white -> panel white, COL-1; no hidden compensation). The exposure is
//      pushed by the CPU: the parameter, or in the physical and mask_depth headroom modes (T-84, D-16) the
//      parameter divided by the headroom readout (src/headroom.rs), so nothing exceeds panel white;
//   2. colour matrix: C = M E, the 3x3 from the tube primaries and white to the sRGB/Rec.709 primaries
//      in linear light (COL-2). The CPU builds M (crt-model); the identity when "match sRGB colours" is
//      on. Pushed as three rows;
//   3. clip counters: pixels with a channel above panel white, and pixels with a negative channel (out
//      of the panel gamut), counted in `clip` (read by the CPU after the frame's fence, the UI hook);
//   4. shoulder (output.glsl, shoulder_rate): identity up to the knee, soft above it up to the white point the
//      CPU solved the rate for (shoulder mode), a hard clip at 1 for knee 1 or rate 0 (the other modes);
//      negative channels clip to 0 (gamut clip in linear light);
//   5. host EOTF inverse chosen by the push constants (pure power, default 2.2, or piecewise sRGB: COL-3);
//   6. dither of 1 LSB of the target format (COL-4, CAP-9) when `levels` > 0 and the mode is not off:
//      code = floor(v levels + threshold), threshold in [0, 1), so the error is below one code and its
//      mean is zero. Float targets (levels = 0) are never dithered.
// Outside the tube rectangle: black surround, not counted.

#include "params.glsl"
#include "common.glsl"
#include "output.glsl"

layout(set = 0, binding = 2) uniform sampler2D linear_in;

// Clip counters, cleared by the CPU before each frame (per frame in flight).
layout(set = 0, binding = 3, std430) buffer Clip {
    uint over_white;
    uint out_of_gamut;
} clip;

layout(push_constant) uniform Push {
    // Rows of the tube -> display matrix (the fourth component is padding).
    vec4 m0;
    vec4 m1;
    vec4 m2;
    uint transfer;
    float gamma;
    // Quantisation steps of the target (255 for 8 bits, 1023 for 10 bits), 0 for a float target.
    float levels;
    // T-84 headroom mode, resolved on the CPU (src/headroom.rs): the effective exposure (the parameter, or the
    // one derived from the headroom readout), the shoulder knee and rate (knee 1 or rate 0: hard clip).
    float exposure;
    float knee;
    float rate;
} pc;

layout(location = 0) out vec4 out_color;

void main() {
    vec2 f = gl_FragCoord.xy;
    vec4 r = frame.tube_rect_px;
    if (f.x < r.x || f.y < r.y || f.x >= r.z || f.y >= r.w) {
        out_color = vec4(0.0, 0.0, 0.0, 1.0);
        return;
    }
    ivec2 q = clamp(ivec2(floor(f) - floor(r.xy)), ivec2(0), frame.tube_size - 1);
    vec3 e = texelFetch(linear_in, q, 0).rgb * pc.exposure;
    vec3 c = vec3(dot(pc.m0.xyz, e), dot(pc.m1.xyz, e), dot(pc.m2.xyz, e));

    if (any(greaterThan(c, vec3(1.0 + CLIP_EPS)))) {
        atomicAdd(clip.over_white, 1u);
    }
    if (any(lessThan(c, vec3(-CLIP_EPS)))) {
        atomicAdd(clip.out_of_gamut, 1u);
    }

    vec3 v = transfer_from_linear(shoulder_rate(c, pc.knee, pc.rate), pc.transfer, pc.gamma);

    if (pc.levels > 0.0 && params.output_dither != OUTPUT_DITHER_OFF) {
        float t = params.output_dither == OUTPUT_DITHER_ORDERED
            ? ordered_threshold(ivec2(f))
            : noise_threshold(f, frame.frame_index);
        v = min(floor(clamp(v, 0.0, 1.0) * pc.levels + t), pc.levels) / pc.levels;
    }
    out_color = vec4(v, 1.0);
}
