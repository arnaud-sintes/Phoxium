// Shadow mask or aperture grille (ARCHITECTURE 3.7, `screen` slot): per-channel emitter weight of host
// pixel q of the tube rectangle, area-averaged over its footprint and normalised so the flat-white area
// mean is 1. T-39a (aperture grille) and T-39b (slot, delta) replace the stub.
//
// `q` as in beam_eval() (beam.glsl).
#ifndef UCRT_MASK_GLSL
#define UCRT_MASK_GLSL

// Stub (T-20b): no mask, weight 1.0 per channel.
vec3 mask_eval(ivec2 q) {
    return vec3(1.0);
}

#endif
