// Phosphor saturation (T-40, CRT.md "Saturation and ageing", QUESTIONS PHY-14; oracle
// crt_model::phosphor::saturate). Include after params.glsl.
//
// Per channel, L = f(I) = I / (1 + s I) for the light I >= 0 of a tube pixel (1 = white), s = the channel
// of params.phosphor_saturation_rgb: f(0) = 0, f'(0) = 1, f' > 0 (monotonic), f < 1 / s. The light of a
// pixel is never negative (a negative input is clamped to 0, which also keeps 1 + s I >= 1). With s = 0 the
// result is I itself. The caller tests params.phosphor_saturation: while it is off nothing is evaluated and
// the stage passes the texels unchanged.
#ifndef UCRT_SATURATION_GLSL
#define UCRT_SATURATION_GLSL

vec3 phosphor_saturate(vec3 light) {
    vec3 i = max(light, vec3(0.0));
    return i / (vec3(1.0) + params.phosphor_saturation_rgb * i);
}

#endif
