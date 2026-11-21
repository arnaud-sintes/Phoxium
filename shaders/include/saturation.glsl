// Phosphor saturation (T-40, CRT.md "Saturation and ageing", QUESTIONS PHY-14; oracle
// crt_model::phosphor::saturate). Include after params.glsl.
//
// Per channel, L = f(I) = I / (1 + s I) for the light I >= 0 of a tube pixel (1 = white), s = the channel
// of params.phosphor_saturation_rgb: f(0) = 0, f'(0) = 1, f' > 0 (monotonic), f < 1 / s. The light of a
// pixel is never negative (a negative input is clamped to 0, which also keeps 1 + s I >= 1). With s = 0 the
// result is I itself. The caller tests params.phosphor_saturation: while it is off nothing is evaluated and
// the stage passes the texels unchanged.
//
// Strength (review R-05, oracle crt_model::phosphor::effective_saturation_strength): s is clamped to
// [0, 1e30] and a NaN reads as 0, so a negative strength from a file or a typed value can never make
// 1 + s I vanish or turn negative (that would give an infinite or negative light); s <= 0 is the identity.
//
// Known deviation (review R-12, listed in DESIGN section 12): I is the area-averaged light of the tube pixel
// after the mask, which the mask normalises by its coverage (shaders/include/mask.glsl), so the effective
// strength depends on the mask tier, the host resolution and the headroom mode (a resolved lit stripe is
// 1 / fill times brighter than the prefiltered mean). Not fixed for now; the fix would saturate the beam
// light before the mask product.
#ifndef PHOXIUM_SATURATION_GLSL
#define PHOXIUM_SATURATION_GLSL

vec3 phosphor_saturate(vec3 light) {
    vec3 i = max(light, vec3(0.0));
    vec3 s = params.phosphor_saturation_rgb;
    // Ordered compare: false for NaN, so NaN and s <= 0 give 0.
    s = mix(vec3(0.0), min(s, vec3(1e30)), greaterThan(s, vec3(0.0)));
    return i / (vec3(1.0) + s * i);
}

#endif
