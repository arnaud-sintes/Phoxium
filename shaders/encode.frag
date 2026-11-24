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
// Outside the tube rectangle: the surround, which this shader does not draw: the draw is scissored to the tube
// rectangle and the rest of the target keeps the attachment's clear colour, `source_surround_color` (black by
// default; `gpu/stages/encode.rs`, T-96). Not counted. With the cabinet on (`cabinet_enable`, cabinet.glsl) the
// draw covers the whole target instead: outside the rectangle it draws the bezel (its linear light through the
// host transfer and the dither, blended over the surround colour by its pixel coverage), and inside it adds the
// bezel's reflection in the glass to the light, before the clip counters.

#include "params.glsl"
#include "common.glsl"
#include "output.glsl"
#include "cabinet.glsl"

layout(set = 0, binding = 2) uniform sampler2D linear_in;

// Clip counters (per frame in flight), in device-local memory: cleared on the GPU before this pass and copied to
// a host-visible buffer after it. They were in host memory until 2026-11-20, where every atomic crossed the bus:
// 3 to 3.5 ms at 1920 x 1080 when most pixels clip (a 1-px checker, noise), 0.13 ms in device memory.
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
    // TATE (`source_rotation`): the tube image is turned a quarter turn into the target; `view_rect` is the
    // tube rectangle as the target shows it (x0, y0, x1, y1), turned with the tube (`placement::viewer_rect`).
    // Offset 80: a vec4 starts on 16 bytes, the Rust block pads after `rate`.
    vec4 view_rect;
} pc;

layout(location = 0) out vec4 out_color;

// Final dither of 1 LSB of the target format (COL-4, CAP-9), shared by the picture and the bezel.
vec3 dither(vec3 v, vec2 f) {
    if (pc.levels > 0.0 && params.output_dither != OUTPUT_DITHER_OFF) {
        float t = params.output_dither == OUTPUT_DITHER_ORDERED
            ? ordered_threshold(ivec2(f))
            : noise_threshold(f, frame.frame_index);
        v = min(floor(clamp(v, 0.0, 1.0) * pc.levels + t), pc.levels) / pc.levels;
    }
    return v;
}

void main() {
    vec2 f = gl_FragCoord.xy;
    vec4 r = params.source_rotation == SOURCE_ROTATION_OFF ? frame.tube_rect_px : pc.view_rect;
    float band = params.cabinet_enable ? cabinet_band(params.cabinet_bezel_width, frame.target_size) : 0.0;
    if (f.x < r.x || f.y < r.y || f.x >= r.z || f.y >= r.w) {
        // Without the cabinet the scissor keeps every fragment inside the tube rectangle and this branch does
        // not run (the surround is the clear colour); with it, this draws the bezel over the surround.
        vec3 surround = params.source_surround_color;
        if (band >= 1.0) {
            vec4 b = cabinet_bezel(f, r, band, params.cabinet_bezel_color, params.cabinet_bevel,
                                   params.cabinet_corner_radius);
            vec3 v = dither(transfer_from_linear(min(b.rgb, vec3(1.0)), pc.transfer, pc.gamma), f);
            surround = mix(surround, v, b.a);
        }
        out_color = vec4(surround, 1.0);
        return;
    }
    ivec2 d = ivec2(floor(f) - floor(r.xy));
    // The target pixel d of the rectangle shows the tube pixel q (the inverse of `placement::viewer_rect`'s turn):
    // counter-clockwise (the tube's top on the left, its right on top), clockwise (its top on the right).
    ivec2 q = d;
    if (params.source_rotation == SOURCE_ROTATION_COUNTER_CLOCKWISE) {
        q = ivec2(frame.tube_size.x - 1 - d.y, d.x);
    } else if (params.source_rotation == SOURCE_ROTATION_CLOCKWISE) {
        q = ivec2(d.y, frame.tube_size.y - 1 - d.x);
    }
    q = clamp(q, ivec2(0), frame.tube_size - 1);
    vec3 e = texelFetch(linear_in, q, 0).rgb * pc.exposure;
    vec3 c = vec3(dot(pc.m0.xyz, e), dot(pc.m1.xyz, e), dot(pc.m2.xyz, e));
    if (band >= 1.0) {
        c += cabinet_reflection(f, r, band, params.cabinet_bezel_color, params.cabinet_bevel,
                                params.cabinet_reflection);
    }

    if (any(greaterThan(c, vec3(1.0 + CLIP_EPS)))) {
        atomicAdd(clip.over_white, 1u);
    }
    if (any(lessThan(c, vec3(-CLIP_EPS)))) {
        atomicAdd(clip.out_of_gamut, 1u);
    }

    vec3 v = transfer_from_linear(shoulder_rate(c, pc.knee, pc.rate), pc.transfer, pc.gamma);
    out_color = vec4(dither(v, f), 1.0);
}
