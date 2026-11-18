// Declarations shared by every pipeline stage (ARCHITECTURE 3.7): the per-frame constants block and the
// named transfer functions (DESIGN 8: conversions to and from gamma or sRGB are explicit functions).
//
// Descriptor convention of every stage, set 0: binding 0 = Params (params.glsl, T-10), binding 1 =
// FrameConstants (below), bindings 2 and up = the stage's own images.
#ifndef UCRT_COMMON_GLSL
#define UCRT_COMMON_GLSL

#ifndef UCRT_FRAME_BINDING
#define UCRT_FRAME_BINDING 1
#endif

// Per-frame constants, written by the CPU once per frame in flight (src/gpu/pipeline.rs,
// `FrameConstants`; a Rust test checks these offsets against the Rust struct). std140, 60 bytes.
layout(std140, set = 0, binding = UCRT_FRAME_BINDING) uniform FrameConstants {
    // Tube rectangle in target pixels: x0, y0, x1, y1 (x1, y1 exclusive). Outside it: black surround.
    layout(offset = 0) vec4 tube_rect_px;
    // Native raster size (source pixels after crop and divisor).
    layout(offset = 16) ivec2 native_size;
    // Output target size in pixels.
    layout(offset = 24) ivec2 target_size;
    // Host pixels per tube millimetre and scanline pitch in mm (T-30, src/placement.rs); 0 only in the
    // unplaced defaults of FrameConstants::new.
    layout(offset = 32) float px_per_mm;
    layout(offset = 36) float line_pitch_mm;
    // Frames rendered since the pipeline was created, and the time since the previous frame in ms.
    layout(offset = 40) uint frame_index;
    layout(offset = 44) float frame_time_ms;
    // Image contract (T-20b, ARCHITECTURE 3.7). Size of the tube-domain images (screen and later):
    // one texel per host pixel of the tube rectangle, whose corners are whole pixels.
    layout(offset = 48) ivec2 tube_size;
    // Horizontal supersampling factor K of the amp-domain images: amp size = native size * (K, 1).
    layout(offset = 56) int amp_k;
} frame;

// Size of the amp-domain images (amp and gun outputs).
ivec2 amp_size() {
    return frame.native_size * ivec2(frame.amp_k, 1);
}

// Transfer functions, selected by the TRANSFER_* constants (src/gpu/pipeline.rs, `Transfer`).
const uint TRANSFER_IDENTITY = 0u; // value unchanged
const uint TRANSFER_POWER = 1u;    // pure power law: L = V^gamma
const uint TRANSFER_SRGB = 2u;     // piecewise sRGB (IEC 61966-2-1) [K]

// Code value V in [0, 1] to linear light L.
float transfer_to_linear(float v, uint kind, float gamma) {
    v = max(v, 0.0);
    if (kind == TRANSFER_POWER) {
        return pow(v, gamma);
    }
    if (kind == TRANSFER_SRGB) {
        return v <= 0.04045 ? v / 12.92 : pow((v + 0.055) / 1.055, 2.4);
    }
    return v;
}

// Linear light L to code value V, the inverse of transfer_to_linear.
float transfer_from_linear(float l, uint kind, float gamma) {
    l = max(l, 0.0);
    if (kind == TRANSFER_POWER) {
        return pow(l, 1.0 / gamma);
    }
    if (kind == TRANSFER_SRGB) {
        return l <= 0.0031308 ? l * 12.92 : 1.055 * pow(l, 1.0 / 2.4) - 0.055;
    }
    return l;
}

vec3 transfer_to_linear(vec3 v, uint kind, float gamma) {
    return vec3(transfer_to_linear(v.r, kind, gamma), transfer_to_linear(v.g, kind, gamma),
                transfer_to_linear(v.b, kind, gamma));
}

vec3 transfer_from_linear(vec3 l, uint kind, float gamma) {
    return vec3(transfer_from_linear(l.r, kind, gamma), transfer_from_linear(l.g, kind, gamma),
                transfer_from_linear(l.b, kind, gamma));
}

#endif
