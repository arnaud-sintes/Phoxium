// Declarations shared by every pipeline stage (ARCHITECTURE 3.7): the per-frame constants block and the
// named transfer functions (DESIGN 8: conversions to and from gamma or sRGB are explicit functions).
//
// Descriptor convention of every stage, set 0: binding 0 = Params (params.glsl, T-10), binding 1 =
// FrameConstants (below), bindings 2 and up = the stage's own images.
#ifndef PHOXIUM_COMMON_GLSL
#define PHOXIUM_COMMON_GLSL

#ifndef PHOXIUM_FRAME_BINDING
#define PHOXIUM_FRAME_BINDING 1
#endif

// Per-frame constants, written by the CPU once per frame in flight (src/gpu/pipeline.rs,
// `FrameConstants`; a Rust test checks these offsets against the Rust struct). std140, 96 bytes, then the 8 KiB target table (the hardware colours of a signal target at entries 0 up, the classification references from entry 256).
layout(std140, set = 0, binding = PHOXIUM_FRAME_BINDING) uniform FrameConstants {
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
    // White balance gains of the gun stage (T-34, src/white.rs): per-channel factors on the beam current,
    // (1, 1, 1) when `gun_white_balance` is off. vec3 alignment: bytes 60 to 63 are padding.
    layout(offset = 64) vec3 white_gain;
    // Margin (host px per side, x and y) of the tube image around the beam raster (T-37): the raster is
    // tube_size - 2 raster_margin, centred, and the margin holds the spot's skirt. 0: the tube image is the
    // raster. Bytes 76 to 79 are padding.
    layout(offset = 80) ivec2 raster_margin;
    // Mask footprint per axis (T-39c, mask.glsl), decided once on the CPU in f64 (src/placement.rs,
    // FrameConstants::with_mask_tiers): 0 = one whole period (prefiltered), 1 = the host pixel (area
    // integral), between = box width in host px (blend band).
    layout(offset = 88) vec2 mask_box_px;
    // The hardware colours of the source colour target when it is a signal one (C64, NES, Atari 8-bit): `rgb` of entry i is the drive
    // voltage of colour i, the first `count` entries (the `target` slot's push constants say how many) are used. The CPU
    // writes them after the 96 bytes above, only while such a target is chosen (src/targets.rs, `frame_table`).
    layout(offset = 96) vec4 target_table[512];
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

// FP16 stores (T-fp16). The Vulkan spec leaves the rounding of a float -> FP16 conversion to the
// implementation (the NVIDIA driver truncates toward zero, other vendors round to nearest), so a chain
// stage that stores through imageStore into an rgba16f image calls round_f16 on the value first: the
// result is already the nearest FP16 value (ties to even, IEEE 754 roundTiesToEven), exactly
// representable in FP16, so the hardware conversion is exact under any rounding mode and the stored
// bits do not depend on the vendor.
//
//   |x| < 2^-14 (FP16 subnormal range, step 2^-24): round(x * 2^24) * 2^-24 with roundEven (the scale
//     is a power of two, so the product is exact in f32); zero keeps its sign, and a value that rounds
//     up to 2^-14 becomes the smallest normal value.
//   normal range: the f32 bits get 0x0FFF plus the lowest kept bit (bit 13) added, then the 13 dropped
//     mantissa bits are masked off; that is round to nearest, ties to even, and a carry into the
//     exponent is the correct renormalisation. Magnitudes that reach 2^16 after rounding (>= 65520)
//     are +-inf, as in IEEE 754; 65504 is the largest finite FP16 value.
//   NaN and inf pass through unchanged (the bit trick would corrupt a NaN payload).
float round_f16(float x) {
    uint b = floatBitsToUint(x);
    if ((b & 0x7F800000u) == 0x7F800000u) {
        return x; // inf or NaN
    }
    if (abs(x) < 6.103515625e-5) { // 2^-14
        return roundEven(x * 16777216.0) * 5.9604644775390625e-8; // 2^24, 2^-24
    }
    uint r = (b + 0x0FFFu + ((b >> 13) & 1u)) & 0xFFFFE000u;
    if ((r & 0x7FFFFFFFu) >= 0x47800000u) { // 2^16: beyond the largest finite FP16 value
        r = (r & 0x80000000u) | 0x7F800000u;
    }
    return uintBitsToFloat(r);
}

vec2 round_f16(vec2 v) {
    return vec2(round_f16(v.x), round_f16(v.y));
}

vec3 round_f16(vec3 v) {
    return vec3(round_f16(v.x), round_f16(v.y), round_f16(v.z));
}

vec4 round_f16(vec4 v) {
    return vec4(round_f16(v.x), round_f16(v.y), round_f16(v.z), round_f16(v.w));
}

#endif