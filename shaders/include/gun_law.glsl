// Gun laws shared by `gun.comp` (drive voltage V -> relative beam current I) and `decode.comp` (the `srgb`
// source mode inverts the nominal curve of the ACTIVE law). T-34 and its review fixes; QUESTIONS PHY-6.
// Needs params.glsl (gun_law, gun_gamma, gun_bt1886_*, gun_annex1_lift) included first.
//
// Laws (CRT.md "Gun law"), I relative: 1 at V = 1.
// - power: I = V^gamma.
// - gog:   I = drive_c * max(gain V + offset + cutoff_c, 0)^gamma (gun.comp only; its NOMINAL curve, the one
//          decode inverts, is the power law, because brightness, contrast, cutoff and drive are tube controls).
// - bt1886: L = a max(V + b, 0)^2.4, a = (Lw^(1/2.4) - Lb^(1/2.4))^2.4, b = Lb^(1/2.4) / (Lw^(1/2.4) -
//          Lb^(1/2.4)); I = L / Lw, computed as (d (V + b))^2.4 / Lw with d = Lw^(1/2.4) - Lb^(1/2.4). With
//          Lb > 0, I(0) = Lb / Lw is not zero (the black floor of the law), and the slope at V = 0 is
//          small but not zero (2.4 a b^1.4 / Lw, about 0.04 for the defaults); the slope is zero only at
//          V = -b, below the black floor. Only the ratio Lb / Lw shapes the curve; `gun_bt1886_white` matters
//          through Lb / Lw alone.
// - bt1886_annex1: k = 1 / (1 + b)^2.6, I = k (V + b)^2.6 for V >= 0.35, I = k (0.35 + b)^-0.4 (V + b)^3.0
//          below. The two branches meet at the knee (value continuous), their slopes do not, by definition
//          of the law (b = 0: 0.559 below, 0.485 above).
//
// Out-of-range guard: gun_gamma is clamped to >= GUN_GAMMA_MIN in both stages (a gamma of 0 or less would
// give pow(0, g) and 1 / g problems); a gamma of exactly 1 stays exactly 1 and returns V bit exact.
#ifndef UCRT_GUN_LAW_GLSL
#define UCRT_GUN_LAW_GLSL

const float GUN_GAMMA_MIN = 0.1;
const float ANNEX1_VC = 0.35;
const float ANNEX1_A1 = 2.6;
const float ANNEX1_A2 = 3.0;

// The exponent of the power and gog laws, clamped (see above).
float gun_gamma() {
    return max(params.gun_gamma, GUN_GAMMA_MIN);
}

float power_law(float v, float gamma) {
    v = max(v, 0.0);
    return gamma == 1.0 ? v : pow(v, gamma);
}

// Inverse of power_law for i in [0, 1].
float power_law_inverse(float i, float gamma) {
    i = max(i, 0.0);
    return gamma == 1.0 ? i : pow(i, 1.0 / gamma);
}

// BT.1886 constants: x = lw (white luminance, > 0), y = d, z = b.
vec3 bt1886_constants() {
    // Domain: 0 <= Lb < Lw. Out of range values are clamped (Lb to 0.999 Lw) instead of giving NaN.
    float lw = max(params.gun_bt1886_white, 1e-6);
    float lb = clamp(params.gun_bt1886_black, 0.0, 0.999 * lw);
    float d = pow(lw, 1.0 / 2.4) - pow(lb, 1.0 / 2.4);
    return vec3(lw, d, pow(lb, 1.0 / 2.4) / d);
}

float bt1886(float v) {
    vec3 c = bt1886_constants();
    return pow(c.y * max(v + c.z, 0.0), 2.4) / c.x;
}

// Inverse of bt1886 for a relative current i. Below the black floor I(0) = Lb / Lw there is no V: the
// result is 0 (the floor is shown).
float bt1886_inverse(float i) {
    vec3 c = bt1886_constants();
    return max(pow(max(i, 0.0) * c.x, 1.0 / 2.4) / c.y - c.z, 0.0);
}

// The knee term (0.35 + b)^-0.4 needs b > -0.35.
float annex1_lift() {
    return max(params.gun_annex1_lift, -0.349);
}

float bt1886_annex1(float v) {
    float b = annex1_lift();
    float k = 1.0 / pow(1.0 + b, ANNEX1_A1);
    float x = max(v + b, 0.0);
    if (v >= ANNEX1_VC) {
        return k * pow(x, ANNEX1_A1);
    }
    return k * pow(ANNEX1_VC + b, ANNEX1_A1 - ANNEX1_A2) * pow(x, ANNEX1_A2);
}

// Inverse of bt1886_annex1 for a relative current i: the branch is chosen by the current at the knee.
float bt1886_annex1_inverse(float i) {
    float b = annex1_lift();
    float k = 1.0 / pow(1.0 + b, ANNEX1_A1);
    i = max(i, 0.0);
    float knee = k * pow(ANNEX1_VC + b, ANNEX1_A1);
    if (i >= knee) {
        return max(pow(i / k, 1.0 / ANNEX1_A1) - b, 0.0);
    }
    float k_low = k * pow(ANNEX1_VC + b, ANNEX1_A1 - ANNEX1_A2);
    return max(pow(i / k_low, 1.0 / ANNEX1_A2) - b, 0.0);
}

// Drive voltage V in [0, 1] whose beam current, through the ACTIVE law's nominal curve, is the relative
// current i (gog: the power law of gun_gamma; the user controls of the gog law are not inverted).
float gun_nominal_inverse(float i) {
    if (params.gun_law == GUN_LAW_BT1886) {
        return bt1886_inverse(i);
    }
    if (params.gun_law == GUN_LAW_BT1886_ANNEX1) {
        return bt1886_annex1_inverse(i);
    }
    return power_law_inverse(i, gun_gamma());
}

#endif
