// Beam profile (ARCHITECTURE 3.7, `screen` slot): beam current per channel deposited on host pixel q of
// the tube rectangle. T-36a: vertical scanline profile, area-integrated over the host pixel footprint.
// T-36b: horizontal spot over the amp samples, spot growth with beam current, per-channel convergence
// hook. T-37: geometry (beam_raster, beam_convergence_px and the host pixel -> raster mapping).
//
// Inputs: `current` is the gun output (amp domain, amp_size(), beam current per channel, 1 = peak drive);
// `q` is the host pixel index in the tube image (0 .. frame.tube_size - 1), whose footprint is [q, q + 1)
// in tube-image pixels, origin at the tube rectangle's top-left; lengths in tube mm use frame.px_per_mm
// and frame.line_pitch_mm. Parameters: params/beam.toml.
//
// Output: area-mean light of the host pixel in units of beam current. Along a scanline the current is
// held over each amp sample (sample j covers [j a, (j + 1) a) host px, a = tube width / amp width: the
// line fills the tube rectangle until T-37). While the beam crosses sample j of line n it carries the
// sample's current i and draws a round spot whose width depends on i (spot growth, per channel: each gun
// has its own current). It deposits i times the line pitch P of light per unit length (one line pitch of
// screen per line), spread by the unit-area spot, so the pixel [qx, qx + 1) x [qy, qy + 1) receives
//   sum over lines n and samples j of  i P X_j(sigma(i)) Y_n(sigma(i))
// with X_j the energy of the sample's box blurred by the spot x-profile that lands in the pixel's columns
// and Y_n the fraction of the spot y-profile in its rows (both exact Gaussian integrals: area
// integration, not point sampling). A flat field of current I has a mean light of I whatever the spot
// width, and each amp sample keeps its energy i a P (PLAN T-36a, T-36b); light is only lost where the
// spot spills past the tube rectangle.
#ifndef UCRT_BEAM_GLSL
#define UCRT_BEAM_GLSL

// Full width at half maximum of a Gaussian in sigmas, 2 sqrt(2 ln 2) [K] (CRT.md, Spot profile: 2.355).
const float BEAM_FWHM_PER_SIGMA = 2.3548200;
// Lines and samples are summed within +/- 4 sigma_max of the host pixel (PLAN T-36a); the Gaussian mass
// beyond 4 sigma is 3.2e-5 per side [K].
const float BEAM_REACH_SIGMAS = 4.0;
// Upper bounds on the lines and on the amp samples per line summed for one host pixel, guards against
// out-of-range parameters (a sigma of 32 line pitches or of 64 samples); in range far fewer are reached.
const int BEAM_MAX_LINES = 256;
const int BEAM_MAX_SAMPLES = 512;
// Normalised beam current at mid drive: drive voltage 0.5 through the reference gun law I = V^2.4, 0.5^2.4
// [D] (CRT.md Gun law; QUESTIONS PHY-7: FWHM at mid drive and at peak, normalised drive). Peak drive is 1.
const float BEAM_MID_CURRENT = 0.18946457;
// The reach of the sums is 4 sigma of the spot at this current, twice the peak [E]: an overdriven sample
// up to it loses at most the 4-sigma tail; above it the tail beyond the reach is cut.
const float BEAM_REACH_CURRENT = 2.0;
// Guards (not reached by physical settings) [E]: smallest FWHM in mm (a spot that shrinks with current) and
// smallest exponent n (n = 0 would divide by zero). The same values are in crt-model (beam.rs).
const float BEAM_MIN_FWHM_MM = 0.001;
const float BEAM_MIN_GROWTH_N = 0.05;

// T-20b stub, kept as the fallback while the tube is not placed (px per mm or line pitch 0, before
// T-30): the nearest amp sample under the pixel centre, an exact identity of the gun output.
vec3 beam_nearest(sampler2D current, ivec2 q) {
    ivec2 n = amp_size();
    ivec2 s = ivec2(floor((vec2(q) + 0.5) * vec2(n) / vec2(frame.tube_size)));
    return texelFetch(current, min(s, n - 1), 0).rgb;
}

// A&S 7.1.26 rational factor: erfc(x) = beam_erfc_poly(x) exp(-x^2) for x >= 0; erf absolute error
// <= 1.5e-7 [K].
vec3 beam_erfc_poly(vec3 a) {
    vec3 t = 1.0 / (1.0 + 0.3275911 * a);
    return t * (0.254829592 + t * (-0.284496736 + t * (1.421413741 + t * (-1.453152027 + t * 1.061405429))));
}

// erf(x) per component, A&S 7.1.26 [K].
vec3 beam_erf(vec3 x) {
    vec3 a = abs(x);
    return sign(x) * (1.0 - beam_erfc_poly(a) * exp(-a * a));
}

// Spot growth (CRT.md, Spot growth with beam current): FWHM^2 = d0^2 + k I^(2n), I the normalised beam
// current. Set by the FWHM at mid drive (BEAM_MID_CURRENT) and at peak (I = 1) and n [E][D]:
// k = (d_peak^2 - d_mid^2) / (1 - x), d0^2 = d_mid^2 - k x, x = I_mid^(2n). When those two widths would
// need a negative floor (d_peak / d_mid > I_mid^-n), n is raised to ln(d_peak / d_mid) / ln(1 / I_mid),
// where d0 = 0 (the pure power law through both widths). d_peak = d_mid is no growth (k = 0).
struct BeamSpot {
    float d0_sq; // mm^2
    float k;     // mm^2 per unit current^(2n)
    float two_n;
};

BeamSpot beam_spot() {
    float dm = params.beam_fwhm_mid_mm;
    float dp = params.beam_fwhm_peak_mm;
    float n = max(params.beam_growth_exponent, BEAM_MIN_GROWTH_N);
    if (dp > dm && dm > 0.0) {
        n = max(n, log(dp / dm) / log(1.0 / BEAM_MID_CURRENT));
    }
    BeamSpot s;
    s.two_n = 2.0 * n;
    float x = pow(BEAM_MID_CURRENT, s.two_n);
    s.k = (dp * dp - dm * dm) / (1.0 - x);
    s.d0_sq = dm * dm - s.k * x;
    return s;
}

// Core sigma of the spot in host pixels for the beam currents `i` (per channel). The spot is round at the
// screen centre (CRT.md, Measured spot sizes: 0.4 x 0.4 mm), so it is the sigma of both axes; position
// dependence (dynamic focus, corner elongation) would split it.
vec3 beam_sigma_px(BeamSpot s, vec3 i) {
    vec3 d_sq = s.d0_sq + s.k * pow(max(i, vec3(0.0)), vec3(s.two_n));
    vec3 d = sqrt(max(d_sq, vec3(BEAM_MIN_FWHM_MM * BEAM_MIN_FWHM_MM)));
    return max(d / BEAM_FWHM_PER_SIGMA * frame.px_per_mm, vec3(1e-4));
}

// Largest core sigma over the currents 0 .. BEAM_REACH_CURRENT (sets the reach of the sums). The law is
// monotonic in I, so it is the larger of the two ends.
float beam_sigma_max_px(BeamSpot s) {
    return max(beam_sigma_px(s, vec3(0.0)).x, beam_sigma_px(s, vec3(BEAM_REACH_CURRENT)).x);
}

// Vertical spot profile: fraction of a unit-area Gaussian of sigma `sigma` (px) between d0 and d1 (px,
// measured from the line centre, d0 <= d1), per channel [K].
vec3 beam_y_fraction(vec3 d0, vec3 d1, vec3 sigma) {
    vec3 k = 0.70710678 / sigma;
    return 0.5 * (beam_erf(d1 * k) - beam_erf(d0 * k));
}

// H(u) = sigma (phi(t) - t Q(t)), t = |u| / sigma, Q = 1 - Phi: the Gaussian part of
// G(u) = u Phi(u / sigma) + sigma phi(u / sigma) = max(u, 0) + H(u), the second antiderivative of the
// unit-area Gaussian [K]. With Q(t) = erfc(t / sqrt 2) / 2 in the A&S form:
// H = sigma exp(-t^2 / 2) (1 / sqrt(2 pi) - t poly / 2). It decays like a Gaussian, so far samples cost no
// cancellation.
vec3 beam_h(vec3 u, vec3 sigma) {
    vec3 t = abs(u) / sigma;
    return sigma * exp(-0.5 * t * t) * (0.39894228 - 0.5 * t * beam_erfc_poly(0.70710678 * t));
}

// Horizontal spot: energy (px) that the sample box [a0, a1) of unit linear density, blurred by a unit-area
// Gaussian of sigma `sigma`, puts in the pixel's columns [b0, b1): the integral of G over the two boxes,
// G(b1 - a0) - G(b1 - a1) - G(b0 - a0) + G(b0 - a1), whose max(u, 0) parts add up to the boxes' overlap.
vec3 beam_x_fraction(vec3 a0, vec3 a1, float b0, float b1, vec3 sigma) {
    vec3 overlap = max(min(a1, vec3(b1)) - max(a0, vec3(b0)), vec3(0.0));
    return overlap + beam_h(b1 - a0, sigma) - beam_h(b1 - a1, sigma) - beam_h(b0 - a0, sigma)
        + beam_h(b0 - a1, sigma);
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
    float h = float(geometry_raster_size().y); // T-37 hook: the raster, inside the tube image's margin
    int rows = max(frame.native_size.y, 1);
    r.mult = int(clamp(round(h / (r.pitch * float(rows))), 1.0, 16.0));
    r.lines = rows * r.mult;
    r.first = 0.5 * (h - float(r.lines) * r.pitch) + 0.5 * r.pitch;
    return r;
}

// Per-channel convergence hook (CRT.md, Convergence, purity and landing): offset (host px) of the landing
// of the red, green and blue beams from the nominal raster, along x (`dx`) and y (`dy`), for the host
// position `p`. Zero until T-37 (static offset plus a quadratic polynomial in x and y, per channel); a
// positive offset moves that channel's picture right or down.
void beam_convergence_px(vec2 p, out vec3 dx, out vec3 dy) {
    geometry_convergence_px(p, dx, dy); // T-37 hook (geometry.glsl)
}

// Light (times 1 / P) of a run of samples of current i covering [a0, a1) on one line, in the pixel
// [x0, x1) x [y0, y1) (e0, e1: the pixel rows from the line centre, per channel): i X Y. Core plus the
// optional wide tail (weight w, sigma `ratio` times the core's, in both axes: the spot is a 2D Gaussian
// core plus a 2D Gaussian tail, CRT.md Spot profile [K][E]). Early out: each component is summed only
// within 4 of the run's own widest sigma over the channels (`far`: distance from the run to the pixel,
// the larger of x and y, less the convergence offsets), the truncation the window gives the widest spot;
// dark runs have narrower spots and cost less.
vec3 beam_run(BeamSpot spot, vec3 i, float a0, float a1, float x0, float x1, vec3 e0, vec3 e1, vec3 dx,
              float far_y, float off_x, float w, float ratio, float widen) {
    if (all(equal(i, vec3(0.0)))) {
        return vec3(0.0);
    }
    vec3 sigma = beam_sigma_px(spot, i);
    float core = BEAM_REACH_SIGMAS * max(sigma.r, max(sigma.g, sigma.b));
    float far = max(far_y, max(x0 - a1, a0 - x1) - off_x);
    if (far > core * widen) {
        return vec3(0.0);
    }
    vec3 s0 = vec3(a0) + dx;
    vec3 s1 = vec3(a1) + dx;
    vec3 f = vec3(0.0);
    if (far <= core) {
        f = beam_x_fraction(s0, s1, x0, x1, sigma) * beam_y_fraction(e0, e1, sigma);
    }
    if (w > 0.0) {
        vec3 st = sigma * ratio;
        f = (1.0 - w) * f + w * beam_x_fraction(s0, s1, x0, x1, st) * beam_y_fraction(e0, e1, st);
    }
    return i * f;
}

// Light of host pixel q: the lines and amp samples within reach (4 widest sigmas plus the convergence
// offsets), each run of equal samples weighted by its spot's horizontal and vertical integrals over the
// pixel (beam_run).
vec3 beam_eval(sampler2D current, ivec2 q) {
    if (!(frame.px_per_mm > 0.0) || !(frame.line_pitch_mm > 0.0)) {
        return beam_nearest(current, q);
    }
    BeamRaster r = beam_raster();
    BeamSpot spot = beam_spot();
    float w = clamp(params.beam_tail_weight, 0.0, 1.0);
    float ratio = max(params.beam_tail_sigma_ratio, 1e-3);
    vec3 dx;
    vec3 dy;
    beam_convergence_px(vec2(q) + 0.5, dx, dy);
    float reach = BEAM_REACH_SIGMAS * beam_sigma_max_px(spot) * (w > 0.0 ? max(1.0, ratio) : 1.0);
    vec3 adx = abs(dx);
    vec3 ady = abs(dy);
    float reach_x = reach + max(adx.r, max(adx.g, adx.b));
    float reach_y = reach + max(ady.r, max(ady.g, ady.b));

    vec2 g0 = geometry_footprint(q); // T-37 hook (geometry.glsl): footprint origin in the raster
    float y0 = g0.y;
    float y1 = y0 + 1.0;
    float n0 = max(0.0, ceil((y0 - reach_y - r.first) / r.pitch));
    float n1 = min(float(r.lines - 1), floor((y1 + reach_y - r.first) / r.pitch));
    n1 = min(n1, n0 + float(BEAM_MAX_LINES - 1));

    int samples = amp_size().x;
    float a = float(geometry_raster_size().x) / float(samples); // T-37 hook: raster width
    float x0 = g0.x;
    float x1 = x0 + 1.0;
    float j0 = max(0.0, floor((x0 - reach_x) / a));
    float j1 = min(float(samples - 1), floor((x1 + reach_x) / a));
    j1 = min(j1, j0 + float(BEAM_MAX_SAMPLES - 1));

    // Runs of equal samples on a line are one box: their spots are the same, and the Gaussian terms at
    // the inner sample boundaries cancel exactly (X of [a0, a1) plus X of [a1, a2) is X of [a0, a2)), so a
    // flat stretch costs one evaluation however many samples it spans.
    float off_x = reach_x - reach;
    float off_y = reach_y - reach;
    float widen = w > 0.0 ? max(1.0, ratio) : 1.0;
    vec3 sum = vec3(0.0);
    for (int n = int(n0); n <= int(n1); n++) {
        float cn = r.first + float(n) * r.pitch;
        float far_y = max(y0 - cn, cn - y1) - off_y;
        vec3 c = vec3(cn) + dy;
        vec3 e0 = y0 - c;
        vec3 e1 = y1 - c;
        int row = n / r.mult;
        vec3 run = vec3(0.0);
        float run0 = j0 * a;
        for (int j = int(j0); j <= int(j1); j++) {
            vec3 i = texelFetch(current, ivec2(j, row), 0).rgb;
            if (j > int(j0)) {
                if (all(equal(i, run))) {
                    continue;
                }
                sum += beam_run(spot, run, run0, float(j) * a, x0, x1, e0, e1, dx, far_y, off_x, w, ratio, widen);
            }
            run = i;
            run0 = float(j) * a;
        }
        sum += beam_run(spot, run, run0, (j1 + 1.0) * a, x0, x1, e0, e1, dx, far_y, off_x, w, ratio, widen);
    }
    return sum * r.pitch;
}

#endif
