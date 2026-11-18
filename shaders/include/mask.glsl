// Shadow mask or aperture grille (ARCHITECTURE 3.7, `screen` slot; A12, A14, PHY-2 option A, PHY-8): per-channel
// emitter weight of host pixel q of the tube rectangle in the product form L_c = M_c B_c (CRT.md, Beam and mask
// interaction). T-39a: aperture grille. T-39b: slot mask and delta (see the switch in mask_eval()).
//
// `q` as in beam_eval() (beam.glsl): host pixel index in the tube image, footprint [q, q + 1) in tube-image
// pixels. Parameters: params/mask.toml. The maths, the conventions and the numbers are those of the CPU oracle
// crates/crt-model/src/mask.rs (T-38), which the tests compare against:
// - the emitter e_c(x, y) in [0, 1] is the luminous shape of the phosphor elements of colour c (black-matrix
//   opening); every edge is a linear ramp whose full 0-to-1 width is centred on the nominal (half-value) edge;
// - the value of a host pixel is the exact area mean of e_c over its footprint divided by the mean coverage of
//   colour c, so a flat white field has mean 1 per channel (no hidden gain: a lit stripe is 1 / coverage);
// - tiers (PHY-2): from 3 host px per triad the area mean itself (tiers A and B); below, the footprint is
//   widened to one full mask period (tier C), which leaves the mean, 1, and the damper wires.
//
// Aperture grille (CRT.md, Aperture grille geometry): vertical stripes of pitch p, R, G, B centred at
// (c + 1/2) p / 3 from the left edge of the tube rectangle, width `mask_stripe_fill` p, edge ramp
// `mask_edge_softness` p; damper-wire shadows are horizontal dark bands at fixed fractions of the visible
// height. The emitter is separable, e_c(x, y) = stripes_c(x) dampers(y), so the area mean of a pixel is the
// product of a column term and a row term, each a closed form (piecewise quadratic).
//
// Numerics: the stripe phase of a column must not drift across the screen (a phase error of 1e-4 triad changes
// a pixel by about 0.1 %, as a slow beat pattern). The horizontal pitch in host px is the f32 product
// px_per_mm * mask_pitch_mm, and the position of column q.x inside its triad is reduced exactly with integer
// arithmetic on the mantissa of that pitch (mask_triad_phase), so every column is as accurate as the first.
// Until T-37 maps host pixels through the tube geometry, the triad grid starts at the left edge of the tube
// rectangle.
#ifndef UCRT_MASK_GLSL
#define UCRT_MASK_GLSL

// Area-integrated mask (tiers A and B) from this many host pixels per triad, prefiltered (tier C) below [D]
// (QUESTIONS PHY-2; CRT.md, Host-display resolution budget; crt-model TIER_B_MIN_PX_PER_TRIAD).
const float MASK_AREA_MIN_PX_PER_TRIAD = 3.0;
// Stripe centres of R, G, B in triad pitches from the start of a triad, (c + 1/2) / 3 [D] (CRT.md, Slot mask
// geometry: each colour p / 3 wide; crt-model stripe_centre).
const vec3 MASK_STRIPE_CENTRE = vec3(1.0 / 6.0, 0.5, 5.0 / 6.0);
// Upper bound on the damper wires summed per pixel, a guard against out-of-range parameters (soft range 0 to 3).
const int MASK_MAX_DAMPERS = 8;

// Integral from minus infinity to t of a linear ramp rising from 0 at t = -s / 2 to 1 at t = s / 2 (s >= 0;
// s = 0 is a hard step): 0, then (t + s / 2)^2 / (2 s), then t.
float mask_ramp_cumulative(float t, float s) {
    float c = clamp(t + 0.5 * s, 0.0, s);
    return c * c / (2.0 * max(s, 1e-30)) + max(t - 0.5 * s, 0.0);
}

vec3 mask_ramp_cumulative(vec3 t, float s) {
    vec3 c = clamp(t + 0.5 * s, 0.0, s);
    return c * c / (2.0 * max(s, 1e-30)) + max(t - 0.5 * s, 0.0);
}

// Integral from minus infinity to r of the trapezoid pulse centred on 0 with nominal (half-value) width w and
// edge ramps of full width s <= w (crt-model pulse_cumulative): the rising ramp at -w / 2 minus the same ramp at
// +w / 2. The total area is w whatever s. Outside the pulse the result is exactly 0 or exactly w, so that the
// difference of two cumulatives taken clear of a pulse is exactly 0 (no rounding noise in the dark guard bands,
// nor in the rows away from a damper wire).
float mask_pulse_cumulative(float r, float w, float s) {
    float reach = 0.5 * (w + s);
    float inside = mask_ramp_cumulative(r + 0.5 * w, s) - mask_ramp_cumulative(r - 0.5 * w, s);
    return r <= -reach ? 0.0 : (r >= reach ? w : inside);
}

vec3 mask_pulse_cumulative(vec3 r, float w, float s) {
    float reach = 0.5 * (w + s);
    vec3 inside = mask_ramp_cumulative(r + 0.5 * w, s) - mask_ramp_cumulative(r - 0.5 * w, s);
    inside = mix(inside, vec3(w), greaterThanEqual(r, vec3(reach)));
    return mix(inside, vec3(0.0), lessThanEqual(r, vec3(-reach)));
}

// True when the mask is area-integrated per host pixel (tiers A and B), false when it is prefiltered (tier C),
// for a mask of `px_per_triad` host pixels per horizontal triad pitch (`mask_tier`, params/mask.toml).
bool mask_area_integrated(float px_per_triad) {
    if (params.mask_tier == MASK_TIER_INTEGRATED) {
        return true;
    }
    if (params.mask_tier == MASK_TIER_PREFILTERED) {
        return false;
    }
    return px_per_triad >= MASK_AREA_MIN_PX_PER_TRIAD;
}

// Position of the left edge of host column `qx` inside its triad, in triad pitches, [0, 1): fract(qx / ppt)
// without rounding drift. ppt = m 2^-s with m its 24-bit mantissa; qx 2^s - k m is computed modulo 2^32 with
// unsigned integers, which is exact because the true remainder is below 2^25 in magnitude (k = floor(qx / ppt)
// from a float division, wrong by at most one, then corrected). The result is the exact remainder divided by m.
float mask_triad_phase(int qx, float ppt) {
    uint bits = floatBitsToUint(ppt);
    int s = 150 - int((bits >> 23) & 0xFFu);
    if (qx < 0 || s < 0 || s > 31) {
        // Pitch of 2^24 px or more, or below 2^-8 px: no drift to speak of, or no mask to speak of.
        return fract(float(qx) / ppt);
    }
    uint m = (bits & 0x7FFFFFu) | 0x800000u;
    uint k = uint(floor(float(qx) / ppt));
    int r = int((uint(qx) << uint(s)) - k * m);
    if (r < 0) {
        r += int(m);
    } else if (r >= int(m)) {
        r -= int(m);
    }
    return float(r) / float(m);
}

// Aperture grille, column term: area mean of the three stripe trains over [a, a + width] (triad pitches, a the
// left edge of the pixel inside its triad), divided by the coverage `fill`. Closed form of crt-model
// train_integral: whole pulses between the two ends, plus the pulse cumulative at each end measured from the
// nearest stripe centre. Needs fill + soft <= 1 / 3 and soft <= fill (mask_grille limits them).
vec3 mask_grille_columns(float a, float width, float fill, float soft) {
    vec3 ta = a - MASK_STRIPE_CENTRE;
    vec3 tb = ta + width;
    vec3 ka = floor(ta + 0.5);
    vec3 kb = floor(tb + 0.5);
    vec3 integral = (kb - ka) * fill
        + (mask_pulse_cumulative(tb - kb, fill, soft) - mask_pulse_cumulative(ta - ka, fill, soft));
    // A pixel between two stripes integrates to 0 up to rounding: never let it go negative.
    return max(integral, 0.0) / (width * fill);
}

// Aperture grille, row term: mean over host row `qy` of the damper factor 1 - sum of the wire shadows
// (crt-model Grille::damper_integral). Wire k of n is at a fixed fraction of the visible height from its top:
// 2/3 for one wire, (k + 1) / (n + 1) otherwise (1/3 and 2/3 for two) [S][E] (CRT.md, Aperture grille geometry;
// crt-model damper_positions_mm). The tube rectangle is the picture, centred in the visible area (T-30,
// src/placement.rs), so its first row is (visible height - rectangle height) / 2 below the top [D].
float mask_damper_row(int qy) {
    int n = min(params.mask_damper_count, MASK_MAX_DAMPERS);
    if (n <= 0) {
        return 1.0;
    }
    float w = max(params.mask_damper_width_mm, 0.0) * frame.px_per_mm;
    float s = clamp(params.mask_damper_edge_mm * frame.px_per_mm, 0.0, w);
    float visible = params.tube_visible_height_mm * frame.px_per_mm;
    float top = 0.5 * (visible - float(frame.tube_size.y));
    float shadow = 0.0;
    for (int k = 0; k < n; k++) {
        float f = n == 1 ? 2.0 / 3.0 : float(k + 1) / float(n + 1);
        float d = float(qy) - (f * visible - top);
        shadow += mask_pulse_cumulative(d + 1.0, w, s) - mask_pulse_cumulative(d, w, s);
    }
    return max(1.0 - shadow, 0.0);
}

// Aperture grille: column term times row term. In tier C the column term is its mean over a full period, 1.
vec3 mask_grille(ivec2 q) {
    float ppt = frame.px_per_mm * params.mask_pitch_mm;
    if (!(ppt > 0.0)) {
        return vec3(1.0);
    }
    float rows = mask_damper_row(q.y);
    if (!mask_area_integrated(ppt)) {
        return vec3(rows);
    }
    float fill = clamp(params.mask_stripe_fill, 1e-4, 1.0 / 3.0);
    float soft = clamp(params.mask_edge_softness, 0.0, min(fill, 1.0 / 3.0 - fill));
    return mask_grille_columns(mask_triad_phase(q.x, ppt), 1.0 / ppt, fill, soft) * rows;
}

// Emitter weight per channel of host pixel q. Exactly 1 when the mask is off or the tube is not placed (px per
// mm 0, before T-30), so the screen slot stays an identity of the beam image there.
vec3 mask_eval(ivec2 q) {
    if (!params.mask_enabled || !(frame.px_per_mm > 0.0)) {
        return vec3(1.0);
    }
    switch (params.mask_type) {
    // T-39b: case MASK_TYPE_SLOT_MASK and case MASK_TYPE_DELTA return their own integrals here. Until then
    // they are drawn as an aperture grille of the same pitch.
    case MASK_TYPE_APERTURE_GRILLE:
    default:
        return mask_grille(q);
    }
}

#endif
