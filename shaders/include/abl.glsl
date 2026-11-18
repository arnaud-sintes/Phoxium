// Automatic beam limiting and raster breathing (T-35, QUESTIONS PHY-13; CRT.md "ABL, blooming and EHT
// regulation"; oracle crates/crt-model/src/abl.rs). Needs params.glsl.
//
// State (one std430 storage buffer, owned by the `abl` slot, src/gpu/stages/abl.rs, set ABL_STATE_SET):
// apl_f = low-passed frame APL (relative beam current, 1 = full-field white), apl_frame = APL of the last
// frame. Cleared to 0 when the slot is created (cold start on black). A shader that declares the block
// includes this file after defining ABL_STATE_QUALIFIER (readonly or empty).

#ifndef ABL_GLSL
#define ABL_GLSL

#define ABL_STATE_SET 1
#define ABL_STATE_BINDING 0

#ifdef ABL_STATE_QUALIFIER
layout(set = ABL_STATE_SET, binding = ABL_STATE_BINDING, std430) ABL_STATE_QUALIFIER buffer AblState {
    float apl_f;
    float apl_frame;
    float pad0;
    float pad1;
} abl_state;
#endif

// Gain slope k in 1/mA: (1/r - 1) / (I_fw - I_th), 0 when the limiter cannot act (I_fw <= I_th).
float abl_k() {
    float span = params.abl_full_white_ma - params.abl_threshold_ma;
    if (span <= 0.0) {
        return 0.0;
    }
    return (1.0 / max(params.abl_full_white_ratio, 1e-3) - 1.0) / span;
}

// Gain g = 1 / (1 + k max(0, x I_fw - I_th)) of the filtered APL x; exactly 1 below the threshold.
float abl_gain(float x) {
    float over = max(x * params.abl_full_white_ma - params.abl_threshold_ma, 0.0);
    return 1.0 / (1.0 + abl_k() * over);
}

// Raster scale 1 + b (x - x_ref) of the filtered APL x (positive b enlarges); exactly 1 for b = 0.
float abl_raster_scale(float x) {
    return 1.0 + params.abl_breathing * (x - params.abl_breathing_ref);
}

#endif
