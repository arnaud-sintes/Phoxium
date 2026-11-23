// Shadow mask or aperture grille (ARCHITECTURE 3.7, `screen` slot; A12, A14, PHY-2 option A, PHY-8): per-channel
// emitter weight of host pixel q of the tube rectangle in the product form L_c = M_c B_c (CRT.md, Beam and mask
// interaction). T-39a: aperture grille. T-39b: slot mask and delta dot mask (mask_slot, mask_delta below).
//
// `q` as in beam_eval() (beam.glsl): host pixel index in the tube image, footprint [q, q + 1) in tube-image
// pixels. Parameters: params/mask.toml. The maths, the conventions and the numbers are those of the CPU oracle
// crates/crt-model/src/mask.rs (T-38), which the tests compare against:
// - the emitter e_c(x, y) in [0, 1] is the luminous shape of the phosphor elements of colour c (black-matrix
//   opening); every edge is a linear ramp whose full 0-to-1 width is centred on the nominal (half-value) edge;
// - the value of a host pixel is the exact area mean of e_c over its footprint divided by the mean coverage of
//   colour c, so a flat white field has mean 1 per channel (no hidden gain: a lit stripe is 1 / coverage);
// - tiers (PHY-2, per axis since T-39c, D-15): the footprint of each axis is the host pixel itself (area
//   integral), a box of one whole period (prefiltered: one triad horizontally, the slot period V or the delta
//   pitch P vertically) or, in the blend band, a box between the two, centred on the pixel. The decision is
//   made once per frame on the CPU in f64 (src/placement.rs, FrameConstants::with_mask_tiers, crt-model
//   axis_tiers) and arrives as frame.mask_box_px: 0 = whole period, else the box width in host px (1 = the
//   pixel). Both periodic axes prefiltered leaves the mean, 1, and the damper wires.
//
// Aperture grille (CRT.md, Aperture grille geometry): vertical stripes of pitch p, R, G, B centred at
// (c + 1/2) p / 3 from the start of each triad, triads starting at the tube centre (below), width `mask_stripe_fill` p, edge ramp
// `mask_edge_softness` p; damper-wire shadows are horizontal dark bands at fixed fractions of the visible
// height. The emitter is separable, e_c(x, y) = stripes_c(x) dampers(y), so the area mean of a pixel is the
// product of a column term and a row term, each a closed form (piecewise quadratic).
//
// Numerics: the stripe phase of a column must not drift across the screen (a phase error of 1e-4 triad changes
// a pixel by about 0.1 %, as a slow beat pattern). The horizontal pitch in host px is the f32 product
// px_per_mm * mask_pitch_mm, and the position of column q.x inside its triad is reduced exactly with integer
// arithmetic on the mantissa of that pitch (mask_triad_phase), so every column is as accurate as the first.
// Origin (T-37): the mask lattice of every type has its origin (a triad start for the grille and the slot
// columns, a slot-period start for the slot rows, a lattice cell corner for the delta) at the tube centre,
// tube_size / 2 of the tube image, on both axes (mask_centred_phase). The mask is fixed to the tube, so its
// phase at the centre does not change with the picture width (PAR, raster aspect) or the raster margin; the
// geometry (geometry.glsl) moves the beam only. The damper wires stay at fractions of the visible height.
#ifndef PHOXIUM_MASK_GLSL
#define PHOXIUM_MASK_GLSL

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

// Footprint width in host px along an axis from its code in frame.mask_box_px (0 = one whole period, given
// here as `period` px; else the box width, 1 = the host pixel), and true when the axis is prefiltered.
bool mask_prefiltered(float code) {
    return !(code > 0.0);
}

float mask_box(float code, float period) {
    return mask_prefiltered(code) ? period : code;
}

// Host LCD subpixels (`mask_host_subpixel`, TODO "host subpixel layout"; crt-model HostSubpixel): the light of
// a host pixel's channel c comes from the panel's own subpixel c, a stripe a third of a pixel wide centred
// (c - 1) / 3 px from the pixel centre on an RGB panel, (1 - c) / 3 on a BGR one. With the layout set, the
// value of channel c is the area mean of the mask over that subpixel's footprint instead of over the whole
// pixel: the same box moved by the offset and narrowed to a third of a pixel when it is the host pixel, so the
// mask is resolved at the panel's own sub-element grain. Off: no offset, the footprint is as before, bit for bit.
vec3 mask_subpixel_shift() {
    switch (params.mask_host_subpixel) {
    case MASK_HOST_SUBPIXEL_RGB:
        return vec3(-1.0, 0.0, 1.0) / 3.0;
    case MASK_HOST_SUBPIXEL_BGR:
        return vec3(1.0, 0.0, -1.0) / 3.0;
    default:
        return vec3(0.0);
    }
}

// Width in host px of the horizontal footprint of a channel: the box `bx` of the tier (code `code`, period
// `period` px), narrowed by two thirds of a pixel at the host pixel (1 px: a third) and by nothing at the
// whole period, linearly in the blend band, so it stays continuous with the prefiltered end. Layout off: bx.
float mask_subpixel_width(float code, float bx, float period) {
    if (params.mask_host_subpixel == MASK_HOST_SUBPIXEL_OFF || mask_prefiltered(code)) {
        return bx;
    }
    float t = period > 1.0 ? clamp((period - bx) / (period - 1.0), 0.0, 1.0) : 1.0;
    return bx - (2.0 / 3.0) * t;
}

// Position of the left edge of host column `qx` inside its triad, in triad pitches, [0, 1): fract(qx / ppt)
// without rounding drift. ppt = m 2^-s with m its 24-bit mantissa; qx 2^s - k m is computed modulo 2^32 with
// unsigned integers, which is exact because the true remainder is below 2^25 in magnitude (k = floor(qx / ppt)
// from a float division, wrong by at most one, then corrected). The result is the exact remainder divided by m.
// A negative qx is reduced as |qx| and reflected exactly (remainder m - r), T-37.
float mask_triad_phase(int qx, float ppt) {
    uint bits = floatBitsToUint(ppt);
    int s = 150 - int((bits >> 23) & 0xFFu);
    if (s < 0 || s > 31) {
        // Pitch of 2^24 px or more, or below 2^-8 px: no drift to speak of, or no mask to speak of.
        return fract(float(qx) / ppt);
    }
    uint ax = uint(abs(qx));
    uint m = (bits & 0x7FFFFFu) | 0x800000u;
    uint k = uint(floor(float(ax) / ppt));
    int r = int((ax << uint(s)) - k * m);
    if (r < 0) {
        r += int(m);
    } else if (r >= int(m)) {
        r -= int(m);
    }
    if (qx < 0 && r != 0) {
        r = int(m) - r;
    }
    return float(r) / float(m);
}

// Position of the left (top) edge of host column (row) q of the tube image inside its mask period `period`,
// in periods, [0, 1), with the mask origin at the tube centre c = size / 2 (T-37: the mask is centred on the
// tube axis, so its phase at the centre does not depend on the picture's width, the PAR or the raster margin):
// fract((q - c) / period) = fract((2 q - size) / (2 period)). Doubling the f32 period is exact (exponent + 1),
// so the integer reduction of mask_triad_phase stays exact for every column, size odd or even.
float mask_centred_phase(int q, int size, float period) {
    return mask_triad_phase(2 * q - size, 2.0 * period);
}

// Aperture grille, column term: area mean of the three stripe trains over [a, a + width] (triad pitches, a the
// left edge of the pixel inside its triad), divided by the coverage `fill`. Closed form of crt-model
// train_integral: whole pulses between the two ends, plus the pulse cumulative at each end measured from the
// nearest stripe centre. `shift`: per-channel offset of the box, in triad pitches (host subpixels). Needs
// fill + soft <= 1 / 3 and soft <= fill (mask_grille limits them).
vec3 mask_grille_columns(float a, vec3 shift, float width, float fill, float soft) {
    vec3 ta = a + shift - MASK_STRIPE_CENTRE;
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
    float top = 0.5 * (visible - float(frame.tube_size.y - 2 * frame.raster_margin.y)); // raster rows (T-37)
    float shadow = 0.0;
    for (int k = 0; k < n; k++) {
        float f = n == 1 ? 2.0 / 3.0 : float(k + 1) / float(n + 1);
        float d = float(qy) - (f * visible - top);
        shadow += mask_pulse_cumulative(d + 1.0, w, s) - mask_pulse_cumulative(d, w, s);
    }
    return max(1.0 - shadow, 0.0);
}

// Aperture grille: column term times row term. Prefiltered horizontally, the column term is its mean over a
// full period, 1; in the blend band it is the mean over a box of mask_box_px.x px centred on the pixel. The
// grille has no vertical period: its rows are always the host row (the damper wires stay in every tier).
// `q` in the tube image: the stripes are centred on the tube centre (mask_centred_phase), the damper rows count
// from the raster's top (inside the margin, T-37).
vec3 mask_grille(ivec2 q) {
    float ppt = frame.px_per_mm * params.mask_pitch_mm;
    if (!(ppt > 0.0)) {
        return vec3(1.0);
    }
    float rows = mask_damper_row(q.y - frame.raster_margin.y);
    if (mask_prefiltered(frame.mask_box_px.x)) {
        return vec3(rows);
    }
    float fill = clamp(params.mask_stripe_fill, 1e-4, 1.0 / 3.0);
    float soft = clamp(params.mask_edge_softness, 0.0, min(fill, 1.0 / 3.0 - fill));
    // Box of bx px centred on the pixel: its left end is (1 - bx) / 2 px from the pixel's (0 for the pixel).
    // Host subpixels: the box is narrowed and moved to each channel's subpixel (mask_subpixel_width).
    float bx = mask_subpixel_width(frame.mask_box_px.x, frame.mask_box_px.x, ppt);
    float a = mask_centred_phase(q.x, frame.tube_size.x, ppt) + 0.5 * (1.0 - bx) / ppt;
    return mask_grille_columns(a, mask_subpixel_shift() / ppt, bx / ppt, fill, soft) * rows;
}

// ---- Slot mask (T-39b) -------------------------------------------------------------------------------------
// CRT.md, Slot mask geometry; crt-model Slot. Triad columns of pitch p with the three stripes of the grille
// (centres (c + 1/2) p / 3, width `mask_stripe_fill` p), cut by bridges of height `mask_slot_bridge` V every
// slot period V = `mask_slot_period` p; column k = floor(x / p) has its bridges centred on k V for even k and
// on V / 2 + k V for odd k (half-period stagger). Every edge, stripe or bridge, is a ramp of full width
// `mask_edge_softness` p. A stripe and its ramps stay inside their column, so the emitter is the sum of two
// separable terms: the stripes of the even columns (a train of period 2 p) times the bridge factor of phase 0,
// plus the odd columns (the same train moved by p) times the bridge factor of phase V / 2. Each factor is a
// closed form, so the area mean is exact. Both coordinates are reduced exactly (mask_triad_phase): x modulo
// 2 p (the f32 pitch doubled, exact) and y modulo V (the f32 product slot period x pitch).

// Integral over [a, a + width] (period units) of a train of trapezoid pulses of period 1 centred on
// centre + k, nominal width w and ramps s (w + s <= 1, s <= w). Same closed form as mask_grille_columns.
float mask_train(float a, float width, float centre, float w, float s) {
    float ta = a - centre;
    float tb = ta + width;
    float ka = floor(ta + 0.5);
    float kb = floor(tb + 0.5);
    return (kb - ka) * w + (mask_pulse_cumulative(tb - kb, w, s) - mask_pulse_cumulative(ta - ka, w, s));
}

vec3 mask_train(float a, float width, vec3 centre, float w, float s) {
    vec3 ta = a - centre;
    vec3 tb = ta + width;
    vec3 ka = floor(ta + 0.5);
    vec3 kb = floor(tb + 0.5);
    return (kb - ka) * w + (mask_pulse_cumulative(tb - kb, w, s) - mask_pulse_cumulative(ta - ka, w, s));
}

vec3 mask_slot(ivec2 q) {
    precise float ppt = frame.px_per_mm * params.mask_pitch_mm;
    precise float v = params.mask_slot_period * ppt;
    if (!(ppt > 0.0) || !(v > 0.0)) {
        return vec3(1.0);
    }
    // Both axes prefiltered: a box of p x V holds a whole period of both column phases, its mean is 1
    // (crt-model prefiltered).
    if (mask_prefiltered(frame.mask_box_px.x) && mask_prefiltered(frame.mask_box_px.y)) {
        return vec3(1.0);
    }
    float fill = clamp(params.mask_stripe_fill, 1e-4, 1.0 / 3.0);
    float bridge = clamp(params.mask_slot_bridge, 1e-4, 1.0 - 1e-4);
    // crt-model validate(): edge <= stripe, stripe + edge <= p / 3, edge <= bridge, bridge + edge <= V.
    float soft = clamp(params.mask_edge_softness * ppt, 0.0,
        min(min(fill, 1.0 / 3.0 - fill) * ppt, min(bridge, 1.0 - bridge) * v));

    // Columns, in units of two triads: even columns hold their stripes at centre / 2, odd ones at (1 + centre) / 2.
    // Footprint: a box of bx px centred on the pixel (one triad when prefiltered horizontally).
    // Host subpixels: the box of each channel is narrowed and moved (mask_subpixel_width, mask_subpixel_shift),
    // which moves the stripe centres the other way.
    float bx = mask_subpixel_width(frame.mask_box_px.x, mask_box(frame.mask_box_px.x, ppt), ppt);
    float a2 = mask_centred_phase(q.x, frame.tube_size.x, 2.0 * ppt) + 0.5 * (1.0 - bx) / (2.0 * ppt);
    float w2 = 0.5 * bx / ppt;
    float s2 = 0.5 * soft / ppt;
    vec3 shift2 = mask_subpixel_shift() / (2.0 * ppt);
    vec3 even = max(mask_train(a2, w2, 0.5 * MASK_STRIPE_CENTRE - shift2, 0.5 * fill, s2), 0.0) / w2;
    vec3 odd = max(mask_train(a2, w2, 0.5 * MASK_STRIPE_CENTRE + 0.5 - shift2, 0.5 * fill, s2), 0.0) / w2;

    // Rows, in slot periods: mean over the footprint's rows of 1 - bridges, bridges centred on 0 (even) or 1/2
    // (odd). Prefiltered vertically (a box of V): 1 - bridge for both phases, in closed form.
    float rows_even = 1.0 - bridge;
    float rows_odd = rows_even;
    if (!mask_prefiltered(frame.mask_box_px.y)) {
        float by = frame.mask_box_px.y;
        float b = mask_centred_phase(q.y, frame.tube_size.y, v) + 0.5 * (1.0 - by) / v;
        float hv = by / v;
        float sv = soft / v;
        rows_even = max(1.0 - max(mask_train(b, hv, 0.0, bridge, sv), 0.0) / hv, 0.0);
        rows_odd = max(1.0 - max(mask_train(b, hv, 0.5, bridge, sv), 0.0) / hv, 0.0);
    }

    // Mean coverage of a colour: fill (1 - bridge) (crt-model coverage).
    return (even * rows_even + odd * rows_odd) / (fill * (1.0 - bridge));
}

// ---- Delta (T-39b) -----------------------------------------------------------------------------------------
// CRT.md, Delta (dot) triad geometry; crt-model Delta. Diagonal pitch P = `mask_pitch_mm`, dots on the
// hexagonal lattice of spacing a = P / sqrt(3): row j at y = j P / 2, dot i of row j at x = (i + 1.5 j) a with
// colour i mod 3. Dot diameter `mask_dot_fill` P, edge ramp `mask_edge_softness` P (radial, full width
// centred on the nominal edge). The pattern repeats over the cell 3 a x P = sqrt(3) P x P (two dots of each
// colour); the horizontal triad pitch 1.5 a = 0.866 P sets the horizontal tier, the row spacing P / 2 the vertical.
//
// Numerics: P is the f32 product px_per_mm x pitch (in host px) and the cell width X the f32 product P x
// sqrt(3); the pixel position is reduced exactly modulo X in x and modulo P in y (mask_triad_phase), so no
// phase drifts. The shader's lattice is therefore X wide and P tall, both f32 values: its aspect differs from
// sqrt(3) by an f32 rounding (about 1e-7), without accumulation.
//
// Area of a soft dot inside the pixel (crt-model soft_dot_rect): the linear ramp is the mean of hard discs of
// radius rho uniform on [R - s / 2, R + s / 2]; rho -> disc-rectangle area is smooth except at the radii where
// the circle touches an edge line or a corner, so [R - s / 2, R + s / 2] is cut there and each piece takes an
// 8-point Gauss-Legendre rule. The disc-rectangle area is exact (inclusion-exclusion of four quadrant terms).

const float MASK_SQRT3 = 1.7320508;
const float MASK_PI = 3.14159265;
// Gauss-Legendre 8-point nodes and weights on [-1, 1] [K].
const float MASK_GL8_X[8] = float[8](-0.96028986, -0.79666648, -0.52553241, -0.18343464, 0.18343464, 0.52553241,
    0.79666648, 0.96028986);
const float MASK_GL8_W[8] = float[8](0.10122854, 0.22238103, 0.31370665, 0.36268378, 0.36268378, 0.31370665,
    0.22238103, 0.10122854);
// Guards on the dot loops, against out-of-range parameters (a 1 px footprint at 0.3 px per triad needs about 9
// rows and 3 dots per row and colour).
const int MASK_MAX_DOT_ROWS = 48;
const int MASK_MAX_DOTS_PER_ROW = 24;

// Antiderivative of sqrt(r^2 - t^2) for 0 <= t <= r: (t h + r^2 atan(t, h)) / 2, h = sqrt(r^2 - t^2). The
// angle is taken from atan2 on (t, h), well conditioned up to t = r, rather than asin(t / r).
float mask_circle_column(float t, float r) {
    float h = sqrt(max((r - t) * (r + t), 0.0));
    return 0.5 * (t * h + r * r * atan(t, h));
}

// Signed area of the disc of radius r > 0 at the origin inside [0, x] x [0, y], odd in x and in y.
float mask_disc_quadrant(float x, float y, float r) {
    if (x == 0.0 || y == 0.0) {
        return 0.0;
    }
    float sgn = sign(x) * sign(y);
    float xa = min(abs(x), r);
    float ya = min(abs(y), r);
    if (xa * xa + ya * ya <= r * r) {
        return sgn * xa * ya;
    }
    float xc = sqrt(max((r - ya) * (r + ya), 0.0));
    return sgn * (ya * xc + mask_circle_column(xa, r) - mask_circle_column(xc, r));
}

// Exact area of the hard disc of radius r centred on the origin inside [x0, x1] x [y0, y1].
float mask_disc_rect(float x0, float x1, float y0, float y1, float r) {
    if (!(r > 0.0)) {
        return 0.0;
    }
    return mask_disc_quadrant(x1, y1, r) - mask_disc_quadrant(x0, y1, r) - mask_disc_quadrant(x1, y0, r)
        + mask_disc_quadrant(x0, y0, r);
}

// Integral over the rectangle [x0, x1] x [y0, y1] (coordinates relative to the dot centre) of a soft dot of
// nominal radius r and ramp s (s <= 2 r).
float mask_soft_dot(float x0, float x1, float y0, float y1, float r, float s) {
    float rmax = r + 0.5 * s;
    if (x0 >= rmax || x1 <= -rmax || y0 >= rmax || y1 <= -rmax) {
        return 0.0;
    }
    if (x0 <= -rmax && x1 >= rmax && y0 <= -rmax && y1 >= rmax) {
        return MASK_PI * (r * r + s * s / 12.0);
    }
    if (!(s > 0.0)) {
        return max(mask_disc_rect(x0, x1, y0, y1, r), 0.0);
    }
    float lo = r - 0.5 * s;
    float cand[8] = float[8](abs(x0), abs(x1), abs(y0), abs(y1), length(vec2(x0, y0)), length(vec2(x0, y1)),
        length(vec2(x1, y0)), length(vec2(x1, y1)));
    float cuts[10];
    cuts[0] = lo;
    cuts[1] = rmax;
    int n = 2;
    for (int i = 0; i < 8; i++) {
        if (cand[i] >= lo && cand[i] <= rmax) {
            cuts[n] = cand[i];
            n++;
        }
    }
    // Insertion sort, at most 10 values.
    for (int i = 1; i < n; i++) {
        float v = cuts[i];
        int k = i - 1;
        while (k >= 0 && cuts[k] > v) {
            cuts[k + 1] = cuts[k];
            k--;
        }
        cuts[k + 1] = v;
    }
    float sum = 0.0;
    for (int i = 0; i + 1 < n; i++) {
        float a = cuts[i];
        float b = cuts[i + 1];
        if (!(b > a)) {
            continue;
        }
        float mid = 0.5 * (a + b);
        float half_width = 0.5 * (b - a);
        float piece = 0.0;
        for (int g = 0; g < 8; g++) {
            piece += MASK_GL8_W[g] * mask_disc_rect(x0, x1, y0, y1, mid + half_width * MASK_GL8_X[g]);
        }
        sum += piece * half_width;
    }
    // A dot just touching the pixel integrates to 0 up to rounding: never let it go negative.
    return max(sum / s, 0.0);
}

vec3 mask_delta(ivec2 q) {
    precise float pp = frame.px_per_mm * params.mask_pitch_mm;
    precise float cell = pp * MASK_SQRT3;
    if (!(pp > 0.0)) {
        return vec3(1.0);
    }
    // Both axes prefiltered: a box of 1.5 a x P (one horizontal triad pitch, cell / 2, by one vertical period)
    // is a whole period of two rows, its mean is 1 (crt-model prefiltered).
    if (mask_prefiltered(frame.mask_box_px.x) && mask_prefiltered(frame.mask_box_px.y)) {
        return vec3(1.0);
    }
    // Footprint: a box of bx x by px centred on the pixel (cell / 2 or P when that axis is prefiltered).
    float bx = mask_subpixel_width(frame.mask_box_px.x, mask_box(frame.mask_box_px.x, 0.5 * cell), 0.5 * cell);
    float by = mask_box(frame.mask_box_px.y, pp);
    vec3 shift = mask_subpixel_shift();
    float a = cell / 3.0;
    float dot_px = clamp(params.mask_dot_fill, 1e-4, 1.0 / MASK_SQRT3) * pp;
    // crt-model validate(): edge <= dot, dot + edge <= a.
    float soft = clamp(params.mask_edge_softness * pp, 0.0, min(dot_px, a - dot_px));
    float r = 0.5 * dot_px;
    float rmax = r + 0.5 * soft;
    float row = 0.5 * pp;

    // Pixel footprint inside its lattice cell, exact phases.
    float x0 = mask_centred_phase(q.x, frame.tube_size.x, cell) * cell + 0.5 * (1.0 - bx);
    float y0 = mask_centred_phase(q.y, frame.tube_size.y, pp) * pp + 0.5 * (1.0 - by);
    float y1 = y0 + by;

    vec3 sum = vec3(0.0);
    int j0 = int(floor((y0 - rmax) / row));
    int j1 = min(int(floor((y1 + rmax) / row)), j0 + MASK_MAX_DOT_ROWS);
    for (int j = j0; j <= j1; j++) {
        float cy = float(j) * row;
        if (y0 - cy >= rmax || y1 - cy <= -rmax) {
            continue;
        }
        for (int c = 0; c < 3; c++) {
            // Dots of colour c in row j: x = (c + 1.5 j) a + m cell. The footprint is moved to the channel's
            // host subpixel (shift[c] px, 0 when the layout is off).
            float xc = (float(c) + 1.5 * float(j)) * a;
            float xl = x0 + shift[c];
            float xr = xl + bx;
            int m0 = int(ceil((xl - rmax - xc) / cell)) - 1;
            int m1 = min(int(floor((xr + rmax - xc) / cell)) + 1, m0 + MASK_MAX_DOTS_PER_ROW);
            for (int m = m0; m <= m1; m++) {
                float cx = xc + float(m) * cell;
                sum[c] += mask_soft_dot(xl - cx, xr - cx, y0 - cy, y1 - cy, r, soft);
            }
        }
    }
    // Mean coverage of a colour: two dots of area pi (R^2 + s^2 / 12) per cell X x P (crt-model coverage).
    float coverage = 2.0 * MASK_PI * (r * r + soft * soft / 12.0) / (cell * pp);
    return sum / (coverage * bx * by);
}

// Emitter weight per channel of host pixel q, the physical mask (mask_eval below adds the switches).
vec3 mask_eval_physical(ivec2 q) {
    // T-37: the mask lattice is centred on the tube centre (mask_centred_phase), which is also the raster's
    // centre (the margin is the same on both sides), so q is taken in the tube image as it is.
    switch (params.mask_type) {
    case MASK_TYPE_SLOT_MASK:
        return mask_slot(q);
    case MASK_TYPE_DELTA:
        return mask_delta(q);
    case MASK_TYPE_APERTURE_GRILLE:
    default:
        return mask_grille(q);
    }
}

// The weight the screen slot multiplies the beam by. Exactly 1 when the mask is off or the tube is not placed
// (px per mm 0, before T-30), so the screen slot stays an identity of the beam image there. In the mask_depth
// headroom mode (T-84, D-16 option C, NON-PHYSICAL) the contrast is scaled: 1 + depth (m - 1) = mix(1, m, depth),
// linear in m, so the field mean (1 per channel) is kept exactly and the peak over the mean falls to
// 1 + depth (peak - 1).
vec3 mask_eval(ivec2 q) {
    if (!params.mask_enabled || !(frame.px_per_mm > 0.0)) {
        return vec3(1.0);
    }
    vec3 m = mask_eval_physical(q);
    if (params.output_headroom_mode == OUTPUT_HEADROOM_MODE_MASK_DEPTH) {
        m = mix(vec3(1.0), m, clamp(params.mask_depth, 0.0, 1.0));
    }
    return m;
}

#endif
