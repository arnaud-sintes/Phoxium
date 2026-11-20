// Curved geometry on the separable beam (T-91; ARCHITECTURE 3.7 `screen` slot; QUESTIONS PHY-11; tests
// tests/stage_beam_warp.rs). Needs params.glsl, common.glsl, abl.glsl, geometry.glsl and beam.glsl (with
// BEAM_NO_TILE: these passes use no tile memory) included before, and the push block `screen_push` { abl_runs,
// ox, pad } (src/gpu/stages/screen.rs, WarpPush).
//
// Why. The standard beam (beam.glsl, T-36c) is separable per tile: pass 1 sums the horizontal profile of each
// line at each column, pass 2 spreads it vertically. A curved faceplate, pincushion, trapezoid or rotation bends
// the raster's lines and columns across a tile, which used to send the standard quality to the per-pixel
// reference form (4.7 to 6.1 times the flat cost, T-47). The beam is still a sum over lines of a horizontal
// profile times a vertical one, in RASTER space: light(q) = sum_n H_n(x0) Y_n([y0, y0 + im.y)), with (x0, y0) the
// raster footprint of host pixel q (geometry_footprint, im the size step's raster px per host px). Only the
// raster position of q bends; the profiles do not. So:
// - pass 1 (screen_lines.comp, beam_lines_entry): the pass-1 entry of the tile path, H and the sigma moments of
//   every raster line n at raster columns on a uniform grid (ox entries per raster px, footprint of width im.x),
//   into the line images below. Same runs, reach and convergence as the tile path, with the horizontal
//   convergence offset taken where the entry lands (geometry_seen_px of its centre on the line), as the tile path
//   takes it at the column and line centre. One entry per (line, column): lines x columns, about 0.6 of the
//   tile path's pass-1 work, which repeats the lines a tile's reach shares with its neighbours and leaves most
//   of its 256 invocations idle in pass 1 (about 9 lines x 8 columns active at 6 px per line). Measured: the
//   warp path costs 0.35 to 0.39 times the flat tile path (presets 01 and 05, tests/stage_beam_warp.rs
//   screen_profile_warp).
// - pass 2 (screen_warp.comp, beam_warp_eval): per host pixel, its raster footprint through the full geometry
//   (geometry_footprint, the same as the reference form), then for each line within reach the entry at its
//   raster x interpolated along the line, and the vertical integral of the tile path's pass 2 (beam_y_line:
//   erf pair plus the second-order spread term), exact in y over the footprint rows. Then mask and opening per
//   host pixel (screen_warp.comp): the mask is fixed to the glass and is never warped.
//
// Sampling (the only approximation added to the tile path's). Vertically there is none: each line's profile is
// integrated analytically over the pixel's raster rows, so the scanlines cannot alias, whatever the magnification
// (a 2D raster-space image resampled at host resolution would put up to 38 % of white of error on a scanline
// peak at 6 px per line and sigma 0.85 px with a bilinear filter, 9 % at twice the vertical resolution, 2.5 % at
// four times; tests/stage_beam_warp.rs W1). Horizontally H_n(x) is the box-filtered (footprint im.x) Gaussian-blurred
// profile of the line, smooth at the scale of the spot sigma; it is sampled every 1 / ox raster px and
// interpolated with Catmull-Rom (4 taps, exact on quadratics and at the grid points: weights (0, 1, 0, 0) at
// f = 0), the moments (sum h sigma^2, sum h sigma^4) linearly with the matching linear H, so the mean and spread
// of sigma^2 are those of the two neighbours' merged runs (never outside their range). The CPU picks ox so that
// the spot sigma at peak current is at least 1.2 grid steps (stages/screen.rs, warp_oversampling): ox 1 down to
// 1.2 raster px, 2 down to 0.6, 4 down to 0.3; below that the standard quality keeps the reference form. The
// interpolation error of a Catmull-Rom on an edge of a box-filtered Gaussian at sigma / step >= 1.2 is at most
// about 1.2 % of white at 6 px per line (full current, no growth; 0.42 % with the PVM-20M4U growth at
// 1440 lines; derivation and table in tests/stage_beam_warp.rs). Flat stretches are exact (the weights sum to
// 1), so a flat field keeps its luminance I / (mx my) exactly as the reference form (Lambertian: the curvature's
// local magnification is not applied, ARCHITECTURE 3.7 geometry bullet).
//
// Line images (owned by the `screen` stage, set 2, rgba16f, alpha unused): width = (tube width + 2 pad - 1) ox
// + 1 columns, height = the raster's lines. Column k is the footprint centred at raster x
// beam_lines_x_start() + k / ox; pad (raster px, >= the reach plus two taps) lets the host pixels at the edge of
// a curved picture, which see raster positions beyond the tube image, find the skirt there. The moments are
// stored divided by sigma_max^2 (and ^4) so FP16 keeps its relative precision for any spot size.
#ifndef UCRT_BEAM_WARP_GLSL
#define UCRT_BEAM_WARP_GLSL

#define BEAM_LINES_SET 2
// Core: sum h, sum h sigma^2 / S, sum h sigma^4 / S^2 (S = sigma_max^2); tail: sum h, sum h sigma^2 / S.
layout(set = BEAM_LINES_SET, binding = 0, rgba16f) uniform image2D beam_lines_h;
layout(set = BEAM_LINES_SET, binding = 1, rgba16f) uniform image2D beam_lines_v;
layout(set = BEAM_LINES_SET, binding = 2, rgba16f) uniform image2D beam_lines_v2;
layout(set = BEAM_LINES_SET, binding = 3, rgba16f) uniform image2D beam_lines_ht;
layout(set = BEAM_LINES_SET, binding = 4, rgba16f) uniform image2D beam_lines_vt;

// Raster x (px from the raster's left edge) of the footprint centre of line-image column 0.
float beam_lines_x_start() {
    return 0.5 - float(frame.raster_margin.x) - float(screen_push.pad);
}

// Pass 1: line-image entry e = (column, line).
void beam_lines_entry(sampler2D current, ivec2 e) {
    ivec2 size = imageSize(beam_lines_h);
    if (any(greaterThanEqual(e, size))) {
        return;
    }
    BeamRaster r = beam_raster();
    BeamSpot spot = beam_spot();
    float w = clamp(params.beam_tail_weight, 0.0, 1.0);
    float smax = beam_sigma_max_px(spot);
    BeamColumn col = BeamColumn(vec3(0.0), vec3(0.0), vec3(0.0), vec3(0.0), vec3(0.0));
    if (e.y < r.lines) {
        float ratio = max(params.beam_tail_sigma_ratio, 1e-3);
        float widen = w > 0.0 ? max(1.0, ratio) : 1.0;
        vec2 im = geometry_inv_magnification();
        float reach = BEAM_REACH_SIGMAS * smax * widen * max(im.x, im.y);
        float xc = beam_lines_x_start() + float(e.x) / float(screen_push.ox);
        float x0 = xc - 0.5 * im.x;
        float cn = r.first + float(e.y) * r.pitch;
        // Horizontal convergence where this entry lands (the tile path: at the column and line centre).
        vec3 cdx;
        vec3 cdy;
        beam_convergence_px(geometry_seen_px(vec2(xc, cn) + vec2(frame.raster_margin), geometry_raster_scale()),
            cdx, cdy);
        cdx *= im.x;
        vec3 adx = abs(cdx);
        float off_x = max(adx.r, max(adx.g, adx.b));
        float reach_x = reach + off_x;
        int samples = amp_size().x;
        float a = float(geometry_raster_size().x) / float(samples);
        float j0 = max(0.0, floor((x0 - reach_x) / a));
        float j1 = min(float(samples - 1), floor((x0 + im.x + reach_x) / a));
        j1 = min(j1, j0 + float(BEAM_MAX_SAMPLES - 1));
        int row = beam_line_row(r, e.y);
        vec3 run = vec3(0.0);
        float run0 = j0 * a;
        for (int j = int(j0); j <= int(j1); j++) {
            vec3 i = texelFetch(current, ivec2(j, row), 0).rgb;
            if (j > int(j0)) {
                if (all(equal(i, run))) {
                    continue;
                }
                beam_run_x(spot, run, run0, float(j) * a, x0, cdx, off_x, w, ratio, widen, im.x, col);
            }
            run = i;
            run0 = float(j) * a;
        }
        beam_run_x(spot, run, run0, (j1 + 1.0) * a, x0, cdx, off_x, w, ratio, widen, im.x, col);
    }
    float inv = 1.0 / (smax * smax);
    imageStore(beam_lines_h, e, round_f16(vec4(col.h, 0.0)));
    imageStore(beam_lines_v, e, round_f16(vec4(col.v * inv, 0.0)));
    imageStore(beam_lines_v2, e, round_f16(vec4(col.v2 * (inv * inv), 0.0)));
    if (w > 0.0) {
        imageStore(beam_lines_ht, e, round_f16(vec4(col.ht, 0.0)));
        imageStore(beam_lines_vt, e, round_f16(vec4(col.vt * inv, 0.0)));
    }
}

// Entry (k, n) of a line image, 0 outside its columns (lw: the width).
#define BEAM_LINES_LOAD(img, k, n) (((k) >= 0 && (k) < lw) ? imageLoad(img, ivec2((k), (n))).rgb : vec3(0.0))

// Pass 2: beam light of host pixel q (tube image), the sum over the lines within reach of the interpolated
// pass-1 entry at the pixel's raster x times the vertical integral over its raster rows (beam_y_line), times the
// line pitch, as the tile path.
vec3 beam_warp_eval(ivec2 q) {
    BeamRaster r = beam_raster();
    BeamSpot spot = beam_spot();
    float w = clamp(params.beam_tail_weight, 0.0, 1.0);
    float ratio = max(params.beam_tail_sigma_ratio, 1e-3);
    float widen = w > 0.0 ? max(1.0, ratio) : 1.0;
    float wc = w > 0.0 ? 1.0 - w : 1.0;
    vec2 im = geometry_inv_magnification();
    float smax = beam_sigma_max_px(spot);
    float reach = BEAM_REACH_SIGMAS * smax * widen * max(im.x, im.y);
    float norm = smax * smax;
    vec3 fallback = beam_sigma_px(spot, vec3(0.0));
    bool growth = spot.k != 0.0;
    float im2 = im.y * im.y;

    vec3 dx;
    vec3 dy;
    beam_convergence_px(vec2(q) + 0.5, dx, dy);
    dy *= im.y;
    vec3 ady = abs(dy);
    float off_y = max(ady.r, max(ady.g, ady.b));
    vec2 g0 = geometry_footprint(q);
    float y0 = g0.y;
    float y1 = y0 + im.y;

    ivec2 lsize = imageSize(beam_lines_h);
    int lw = lsize.x;
    float t = (g0.x + 0.5 * im.x - beam_lines_x_start()) * float(screen_push.ox);
    if (!(t > -2.0 && t < float(lw) + 1.0)) {
        return vec3(0.0); // all four taps outside the line image (or the ray missed the faceplate)
    }
    float kf = floor(t);
    float f = t - kf;
    int k = int(kf);
    // Catmull-Rom weights of the taps k - 1 .. k + 2 at the fraction f.
    vec4 cw = vec4(((-0.5 * f + 1.0) * f - 0.5) * f, (1.5 * f - 2.5) * f * f + 1.0, ((-1.5 * f + 2.0) * f + 0.5) * f,
        (0.5 * f - 0.5) * f * f);

    float reach_y = reach + off_y;
    float n0 = max(0.0, ceil((y0 - reach_y - r.first) / r.pitch));
    float n1 = min(float(min(r.lines, lsize.y) - 1), floor((y1 + reach_y - r.first) / r.pitch));
    n1 = min(n1, n0 + float(BEAM_MAX_LINES - 1));
    vec3 sum = vec3(0.0);
    for (int n = int(n0); n <= int(n1); n++) {
        float cn = r.first + float(n) * r.pitch;
        float far_y = max(y0 - cn, cn - y1) - off_y;
        vec3 c = vec3(cn) + dy;
        vec3 e0 = y0 - c;
        vec3 e1 = y1 - c;
        vec3 h1 = BEAM_LINES_LOAD(beam_lines_h, k, n);
        vec3 h2 = BEAM_LINES_LOAD(beam_lines_h, k + 1, n);
        vec3 h = cw.x * BEAM_LINES_LOAD(beam_lines_h, k - 1, n) + cw.y * h1 + cw.z * h2
            + cw.w * BEAM_LINES_LOAD(beam_lines_h, k + 2, n);
        vec3 hl = mix(h1, h2, f);
        vec3 s = fallback;
        vec3 var = vec3(0.0);
        if (growth) {
            // The tile path's moments (beam_eval_workgroup) of the linearly merged neighbours.
            vec3 v = mix(BEAM_LINES_LOAD(beam_lines_v, k, n), BEAM_LINES_LOAD(beam_lines_v, k + 1, n), f) * norm;
            vec3 v2 = mix(BEAM_LINES_LOAD(beam_lines_v2, k, n), BEAM_LINES_LOAD(beam_lines_v2, k + 1, n), f)
                * (norm * norm);
            vec3 hs = max(hl, vec3(1e-30));
            vec3 m = v / hs;
            s = beam_mean_sigma(hl, v, fallback);
            var = max(v2 / hs - m * m, vec3(0.0));
        }
        sum += beam_y_line(max(h, vec3(0.0)) * wc, s * im.y, var * (im2 * im2), e0, e1, far_y);
        if (w > 0.0) {
            vec3 t1 = BEAM_LINES_LOAD(beam_lines_ht, k, n);
            vec3 t2 = BEAM_LINES_LOAD(beam_lines_ht, k + 1, n);
            vec3 ht = cw.x * BEAM_LINES_LOAD(beam_lines_ht, k - 1, n) + cw.y * t1 + cw.z * t2
                + cw.w * BEAM_LINES_LOAD(beam_lines_ht, k + 2, n);
            vec3 vt = mix(BEAM_LINES_LOAD(beam_lines_vt, k, n), BEAM_LINES_LOAD(beam_lines_vt, k + 1, n), f) * norm;
            vec3 st = beam_mean_sigma(mix(t1, t2, f), vt * ratio * ratio, fallback * ratio) * im.y;
            sum += beam_y_line(max(ht, vec3(0.0)) * w, st, vec3(0.0), e0, e1, far_y);
        }
    }
    return sum * r.pitch;
}

#endif
