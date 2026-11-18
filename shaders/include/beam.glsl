// Beam profile (ARCHITECTURE 3.7, `screen` slot): beam current per channel deposited on host pixel q of
// the tube rectangle. T-36a: vertical scanline profile, area-integrated over the host pixel footprint.
// T-36b: horizontal spot, spot growth, convergence (beam_line_current, beam_sigma_v_px). T-37: geometry
// (beam_raster and the host pixel -> raster mapping).
//
// Inputs: `current` is the gun output (amp domain, amp_size(), beam current per channel); `q` is the
// host pixel index in the tube image (0 .. frame.tube_size - 1), whose footprint is [q, q + 1) in
// tube-image pixels, origin at the tube rectangle's top-left; lengths in tube mm use frame.px_per_mm and
// frame.line_pitch_mm. Parameters: params/beam.toml.
//
// Output: area-mean light of the host pixel in units of beam current. Each scanline deposits its current
// times the line pitch per unit length (one line pitch of screen per line), spread vertically by the
// unit-area spot profile, so a flat field of current I has a mean light of I whatever the spot width
// (PLAN T-36a: energy normalised, a flat field's mean luminance does not depend on sigma).
#ifndef UCRT_BEAM_GLSL
#define UCRT_BEAM_GLSL

// Full width at half maximum of a Gaussian in sigmas, 2 sqrt(2 ln 2) [K] (CRT.md, Spot profile: 2.355).
const float BEAM_FWHM_PER_SIGMA = 2.3548200;
// Lines are summed within +/- 4 sigma_max of the host pixel (PLAN T-36a); the Gaussian mass beyond
// 4 sigma is 6.3e-5 [K].
const float BEAM_REACH_SIGMAS = 4.0;
// Upper bound on the lines summed for one host pixel, a guard against out-of-range parameters (a
// sigma of 32 line pitches); in range (FWHM <= 3 mm, tail ratio <= 10) far fewer lines are reached.
const int BEAM_MAX_LINES = 256;

// T-20b stub, kept as the fallback while the tube is not placed (px per mm or line pitch 0, before
// T-30): the nearest amp sample under the pixel centre, an exact identity of the gun output.
vec3 beam_nearest(sampler2D current, ivec2 q) {
    ivec2 n = amp_size();
    ivec2 s = ivec2(floor((vec2(q) + 0.5) * vec2(n) / vec2(frame.tube_size)));
    return texelFetch(current, min(s, n - 1), 0).rgb;
}

// erf(x), Abramowitz and Stegun 7.1.26, absolute error <= 1.5e-7 [K].
float beam_erf(float x) {
    float a = abs(x);
    float t = 1.0 / (1.0 + 0.3275911 * a);
    float poly = t * (0.254829592 + t * (-0.284496736 + t * (1.421413741 + t * (-1.453152027 + t * 1.061405429))));
    return sign(x) * (1.0 - poly * exp(-a * a));
}

// Fraction of a unit-area Gaussian of sigma 1 / (k sqrt 2) between d0 and d1 (d0 <= d1).
float beam_gauss_fraction(float d0, float d1, float k) {
    return 0.5 * (beam_erf(d1 * k) - beam_erf(d0 * k));
}

// Vertical spot profile (T-36a): fraction of one scanline's energy that lands between d0 and d1 (px,
// measured from the line centre, d0 <= d1). Gaussian core of sigma `sigma` (px) plus a wide Gaussian
// tail of weight beam_tail_weight and sigma beam_tail_sigma_ratio * sigma, unit area in total [K][E]
// (CRT.md, Spot profile).
float beam_v_fraction(float d0, float d1, float sigma) {
    float k = 0.70710678 / max(sigma, 1e-4);
    float core = beam_gauss_fraction(d0, d1, k);
    float w = clamp(params.beam_tail_weight, 0.0, 1.0);
    if (w <= 0.0) {
        return core;
    }
    float tail = beam_gauss_fraction(d0, d1, k / max(params.beam_tail_sigma_ratio, 1e-3));
    return (1.0 - w) * core + w * tail;
}

// Core sigma of the vertical profile in host pixels for a line of beam current `i`. T-36a: no spot
// growth, sigma = FWHM at mid drive / 2.3548, converted with px per mm. T-36b adds growth with current.
float beam_sigma_v_px(float i) {
    return params.beam_fwhm_mid_mm / BEAM_FWHM_PER_SIGMA * frame.px_per_mm;
}

// Largest core sigma over all beam currents (sets the reach of the line sum). T-36a: the constant one.
float beam_sigma_max_px() {
    return beam_sigma_v_px(0.0);
}

// The scanline raster in tube-image pixels (until T-37 maps host pixels through the geometry): it fills
// the tube rectangle vertically, centred. Line pitch P = line_pitch_mm * px_per_mm (T-30). The line
// multiplier m (T-30 `line_multiplier`, each native row drawn m times) is recovered as
// round(tube height / (P * native rows)), clamped to [1, 16]; the raster has native rows * m lines; line n is
// centred at first + n P, first = (tube height - lines P) / 2 + P / 2, and draws native row n / m.
struct BeamRaster {
    float pitch;
    float first;
    int lines;
    int mult;
};

BeamRaster beam_raster() {
    BeamRaster r;
    r.pitch = frame.line_pitch_mm * frame.px_per_mm;
    float h = float(frame.tube_size.y);
    int rows = max(frame.native_size.y, 1);
    r.mult = int(clamp(round(h / (r.pitch * float(rows))), 1.0, 16.0));
    r.lines = rows * r.mult;
    r.first = 0.5 * (h - float(r.lines) * r.pitch) + 0.5 * r.pitch;
    return r;
}

// Beam current of scanline `row` (native row) seen by host column qx. T-36a: the nearest amp sample under
// the pixel centre (image contract, the T-20b rule). T-36b replaces it by the horizontal spot integrated
// over the pixel footprint, with the per-channel convergence offset.
vec3 beam_line_current(sampler2D current, int row, int qx) {
    ivec2 n = amp_size();
    int sx = int(floor((float(qx) + 0.5) * float(n.x) / float(frame.tube_size.x)));
    return texelFetch(current, ivec2(min(sx, n.x - 1), row), 0).rgb;
}

// Light of host pixel q: the scanlines within reach, each weighted by the fraction of its profile that
// falls in the pixel's rows [q.y, q.y + 1), times the line pitch (energy per line, see the header).
vec3 beam_eval(sampler2D current, ivec2 q) {
    if (!(frame.px_per_mm > 0.0) || !(frame.line_pitch_mm > 0.0)) {
        return beam_nearest(current, q);
    }
    BeamRaster r = beam_raster();
    float reach = BEAM_REACH_SIGMAS * beam_sigma_max_px()
        * max(1.0, params.beam_tail_weight > 0.0 ? params.beam_tail_sigma_ratio : 1.0);
    float y0 = float(q.y);
    float y1 = y0 + 1.0;
    float n0 = max(0.0, ceil((y0 - reach - r.first) / r.pitch));
    float n1 = min(float(r.lines - 1), floor((y1 + reach - r.first) / r.pitch));
    n1 = min(n1, n0 + float(BEAM_MAX_LINES - 1));
    vec3 sum = vec3(0.0);
    for (int n = int(n0); n <= int(n1); n++) {
        float c = r.first + float(n) * r.pitch;
        vec3 i = beam_line_current(current, n / r.mult, q.x);
        sum += i * beam_v_fraction(y0 - c, y1 - c, beam_sigma_v_px(max(i.r, max(i.g, i.b))));
    }
    return sum * r.pitch;
}

#endif
