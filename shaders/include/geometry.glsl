// Geometry and convergence (T-37, ARCHITECTURE 3.7 `screen` slot; QUESTIONS PHY-11; CRT.md Geometry; parameters
// params/geometry.toml; oracle crates/crt-model/src/geometry.rs). Needs params.glsl, common.glsl and abl.glsl (with
// ABL_STATE_QUALIFIER defined, for raster_scale), included before beam.glsl, which calls the two hooks below:
// - geometry_footprint(q): where the beam raster is sampled for host pixel q (beam_eval, its footprint origin);
// - geometry_convergence_px(p, dx, dy): per-channel misconvergence (beam_convergence_px).
// The mask (mask.glsl) is not moved: it stays in tube coordinates, the host pixels of the tube rectangle.
//
// Coordinates. The tube rectangle is the visible picture, centred on the tube axis; its centre c = tube_size / 2
// (host px) is the tube centre. A host position p is the point seen by the eye; u = (p - c) / px_per_mm is its
// position in mm on the plane tangent to the faceplate at its centre. The beam raster is laid on the phosphor
// surface uniform in arc length (CRT.md Raster distortions: "for a sphere centred on the deflection centre,
// position = R theta", S-correction makes the screen speed constant), so the raster coordinate of the point is
// its arc length from the centre along the surface. Steps, from the eye to the raster:
// 1. curvature: the ray from the eye (on the axis at geometry_eye_distance_mm from the faceplate centre, or at
//    infinity for 0) through u hits the surface (sphere of radius R centred on the axis R behind the centre, or
//    cylinder of horizontal radius R with a vertical axis); the arc length to that point, R atan(rho, R - z),
//    along the lateral direction (spherical) or along x with y unchanged (cylindrical). No refraction.
// 2. rotation (inverse): the landing point turned back by -angle about the centre.
// 3. trapezoid (inverse, exact): x / (1 + t y / half height); y is unchanged by the trapezoid.
// 4. pincushion (inverse): the forward residual is s (1 + k |s|^2 / rc^2) (rc the half diagonal); inverted by
//    four fixed-point steps (contraction about 2|k| (r / rc)^2 <= 0.1, each step cuts the error 8 to 11x; at
//    |k| = 0.05 in the corner of a 386 x 290 mm picture the residual is 0.0006 mm (k > 0) to 0.0028 mm (k < 0),
//    0.003 to 0.014 px at 1440 lines, against 0.007 to 0.023 mm with three steps; T-37 review, f64 check).
// 5. size: the nominal raster is scaled by (1 + overscan) per axis and by raster_scale (ABL breathing, T-35)
//    about the tube centre, so the raster point is the landing point divided by those factors.
// Identity (every parameter at its default, raster_scale 1): geometry_footprint returns vec2(q) exactly through
// a uniform branch (no cost, bit-identical to the T-36b render).
//
// Footprint. The beam is integrated over the host pixel's footprint moved to the raster position of its centre,
// of size 1 / magnification of the size step (geometry_inv_magnification, T-37 review F3: overscan and
// raster_scale spread the beam, dimmer by 1 / (mx my), spot size kept in mm); the curvature does not change it:
// the surface is Lambertian, so a compressed area keeps its luminance (a curved edge shows less raster, as
// bright). The curvature's local stretch (at most 1 / cos of the corner angle, 1.6 % for a 1R corner) is not
// applied to the footprint.
#ifndef UCRT_GEOMETRY_GLSL
#define UCRT_GEOMETRY_GLSL

const float GEOMETRY_DEG = 0.017453292;

// HOOK (beam_raster, beam_eval): size (host px) of the beam raster. The tube image is the raster grown by
// frame.raster_margin on each side (the margin holds the spot's skirt past the raster's ends); both share the
// tube centre. Raster coordinates (beam.glsl) start at the raster's top-left corner.
ivec2 geometry_raster_size() {
    return frame.tube_size - 2 * frame.raster_margin;
}

// raster_scale = 1 + abl_breathing (apl_f - abl_breathing_ref), read from the ABL state the `abl` slot left for
// this frame (its reduce pass runs before `screen`; src/gpu/stages/screen.rs binds that buffer as set 1).
// Exactly 1 when breathing is off or when the `abl` slot does not run in this frame (disabled or skipped: its
// state is stale or was never written; GEOMETRY_ABL_RUNS, a push constant of screen.comp set by the CPU), in
// both cases without reading the buffer. The state starts at apl_f = abl_breathing_ref (src/gpu/stages/abl.rs),
// so the first frame has scale exactly 1. A value out of [0.5, 2] is ignored (a guard only).
#ifndef GEOMETRY_ABL_RUNS
#define GEOMETRY_ABL_RUNS false
#endif
float geometry_raster_scale() {
    if (params.abl_breathing == 0.0 || !(GEOMETRY_ABL_RUNS)) {
        return 1.0;
    }
    float s = abl_raster_scale(abl_state.apl_f);
    return (s >= 0.5 && s <= 2.0) ? s : 1.0;
}

// True when the mapping is the identity (uniform over the dispatch).
bool geometry_identity(float scale) {
    return params.geometry_curvature == GEOMETRY_CURVATURE_FLAT && params.geometry_overscan_x == 0.0
        && params.geometry_overscan_y == 0.0 && params.geometry_pincushion == 0.0
        && params.geometry_trapezoid == 0.0 && params.geometry_rotation_deg == 0.0 && scale == 1.0;
}

// Step 1: arc-length position (mm, from the faceplate centre) of the surface point seen through the tangent-plane
// point u (mm). Returns false when the ray misses the surface (seen beyond its rim).
bool geometry_surface_arc(vec2 u, out vec2 arc) {
    arc = u;
    float r = params.geometry_radius_mm;
    if (params.geometry_curvature == GEOMETRY_CURVATURE_FLAT || !(r > 0.0)) {
        return true;
    }
    float d = params.geometry_eye_distance_mm;
    bool cyl = params.geometry_curvature == GEOMETRY_CURVATURE_CYLINDRICAL;
    // Ray origin o and direction v in a frame with the faceplate centre at 0, z into the tube; the centre of
    // curvature is at (0, 0, r). Eye at infinity: o = (u, 0), v = (0, 0, 1).
    vec3 o = d > 0.0 ? vec3(0.0, 0.0, -d) : vec3(u, 0.0);
    vec3 v = d > 0.0 ? normalize(vec3(u, d)) : vec3(0.0, 0.0, 1.0);
    // Surface |m (o + t v) - (0, 0, r)| = r with m = (1, 1, 1) (sphere) or (1, 0, 1) (cylinder, vertical axis).
    vec3 m = cyl ? vec3(1.0, 0.0, 1.0) : vec3(1.0);
    vec3 om = o * m - vec3(0.0, 0.0, r);
    vec3 vm = v * m;
    float a = dot(vm, vm);
    float b = dot(om, vm);
    float c = dot(o.xy * m.xy, o.xy * m.xy) + o.z * (o.z - 2.0 * r);
    float disc = b * b - a * c;
    if (!(disc >= 0.0) || !(a > 0.0)) {
        return false;
    }
    // Nearest hit; c / (-b + sqrt(disc)) is the cancellation-free form of (-b - sqrt(disc)) / a (b < 0 here).
    float t = c / (-b + sqrt(disc));
    vec3 h = o + t * v;
    if (cyl) {
        arc = vec2(r * atan(h.x, r - h.z), h.y);
    } else {
        float rho = length(h.xy);
        arc = rho > 0.0 ? h.xy * (r * atan(rho, r - h.z) / rho) : vec2(0.0);
    }
    return true;
}

// Raster position (host px of the tube rectangle) of the host position p (host px).
vec2 geometry_raster_px(vec2 p, float scale) {
    float ppm = frame.px_per_mm;
    vec2 c = 0.5 * vec2(frame.tube_size);
    vec2 half_mm = 0.5 * vec2(geometry_raster_size()) / ppm;
    vec2 s;
    if (!geometry_surface_arc((p - c) / ppm, s)) {
        return vec2(-1e6); // far outside the raster: no line or sample within reach, black

    }
    // 2. Rotation, inverse.
    float ang = params.geometry_rotation_deg * GEOMETRY_DEG;
    if (ang != 0.0) {
        float co = cos(ang);
        float si = sin(ang);
        s = vec2(co * s.x + si * s.y, -si * s.x + co * s.y);
    }
    // 3. Trapezoid, inverse.
    s.x /= 1.0 + params.geometry_trapezoid * s.y / half_mm.y;
    // 4. Pincushion, inverse (fixed point).
    float k = params.geometry_pincushion;
    if (k != 0.0) {
        float rc2 = dot(half_mm, half_mm);
        vec2 land = s;
        for (int i = 0; i < 4; i++) {
            s = land / (1.0 + k * dot(s, s) / rc2);
        }
    }
    // 5. Size: overscan per axis and raster_scale.
    s /= vec2(1.0 + params.geometry_overscan_x, 1.0 + params.geometry_overscan_y) * scale;
    return c + s * ppm;
}

// Forward map (T-91): the host position (tube image px) where the raster point r (tube image px: raster px from
// the raster's top-left plus raster_margin) lands and is seen, the inverse of geometry_raster_px, step by step
// in the opposite order: size, pincushion s (1 + k |s|^2 / rc^2), trapezoid x (1 + t y / half height), rotation
// by +angle, then the curvature (arc s at angle phi = |s| / R on the surface: lateral R sin(phi), depth
// R (1 - cos(phi)), seen at lateral x eye / (eye + depth), or the lateral itself with the eye at infinity;
// cylindrical: along x only, y kept on the surface and scaled by the same perspective). Used where a raster
// point needs its landing position (the convergence of a line-image entry, beam_warp.glsl); f32, not used for
// any position the tests measure.
vec2 geometry_seen_px(vec2 r, float scale) {
    float ppm = frame.px_per_mm;
    vec2 c = 0.5 * vec2(frame.tube_size);
    vec2 half_mm = 0.5 * vec2(geometry_raster_size()) / ppm;
    vec2 s = (r - c) / ppm;
    s *= vec2(1.0 + params.geometry_overscan_x, 1.0 + params.geometry_overscan_y) * scale;
    float k = params.geometry_pincushion;
    if (k != 0.0) {
        s *= 1.0 + k * dot(s, s) / dot(half_mm, half_mm);
    }
    s.x *= 1.0 + params.geometry_trapezoid * s.y / half_mm.y;
    float ang = params.geometry_rotation_deg * GEOMETRY_DEG;
    if (ang != 0.0) {
        float co = cos(ang);
        float si = sin(ang);
        s = vec2(co * s.x - si * s.y, si * s.x + co * s.y);
    }
    float rr = params.geometry_radius_mm;
    if (params.geometry_curvature != GEOMETRY_CURVATURE_FLAT && rr > 0.0) {
        float d = params.geometry_eye_distance_mm;
        bool cyl = params.geometry_curvature == GEOMETRY_CURVATURE_CYLINDRICAL;
        float rho = cyl ? abs(s.x) : length(s);
        if (rho > 0.0) {
            float phi = rho / rr;
            float z = rr * (1.0 - cos(phi));
            float lat = rr * sin(phi) / rho; // lateral per unit arc
            float persp = d > 0.0 ? d / (d + z) : 1.0;
            s = cyl ? vec2(s.x * lat, s.y) * persp : s * (lat * persp);
        }
    }
    return c + s * ppm;
}

// HOOK (beam_eval): origin of the footprint of host pixel q (tube image) in the beam raster (host px from the
// raster's top-left). q - raster_margin exactly when the geometry is the identity or the tube is not placed.
vec2 geometry_footprint(ivec2 q) {
    float scale = geometry_raster_scale();
    if (geometry_identity(scale) || !(frame.px_per_mm > 0.0)) {
        return vec2(q - frame.raster_margin);
    }
    vec2 im = 1.0 / (vec2(1.0 + params.geometry_overscan_x, 1.0 + params.geometry_overscan_y) * scale);
    return geometry_raster_px(vec2(q) + 0.5, scale) - 0.5 * im - vec2(frame.raster_margin);
}

// HOOK (beam_eval, T-37 review F3): raster px per host px along x and y, the inverse of the size step's
// magnification (1 + overscan) raster_scale. A larger raster spreads the same beam over more screen: the spot
// keeps its size in mm (its sigma and the convergence offsets, in host px, are multiplied by this to get raster
// px) and the host pixel's footprint in the raster is this size (instead of 1 x 1), so the same current lands
// on a larger area and the picture is dimmer by 1 / (mx my) (CRT.md ABL, blooming and EHT regulation: raster
// size and brightness change together at constant beam current). Exactly (1, 1) for the identity. The local
// magnification of the curvature (Lambertian surface, see Footprint above) and of the pincushion, trapezoid
// and rotation residuals (anisotropic, up to 1 + 3|k| radially in the corner) is not applied.
vec2 geometry_inv_magnification() {
    float scale = geometry_raster_scale();
    if (geometry_identity(scale) || !(frame.px_per_mm > 0.0)) {
        return vec2(1.0);
    }
    return 1.0 / (vec2(1.0 + params.geometry_overscan_x, 1.0 + params.geometry_overscan_y) * scale);
}

// True when the host -> raster mapping is separable per axis (uniform over the dispatch, parameters only): flat
// faceplate, no rotation, trapezoid or pincushion. Overscan, raster_scale and the margin are then per-axis affine
// maps (the raster x of a host pixel depends on its host x only, its raster y on its host y only, both
// increasing), so the separable standard-quality beam (beam.glsl, T-36c) stays exact; any other geometry takes the
// two-pass warp path of the standard quality (T-91, beam_warp.glsl, decided on the CPU by
// src/gpu/stages/screen.rs, which mirrors this test). The radial convergence term is not checked: it is not part of
// the host -> raster map but an offset the standard path takes at the column and line centre instead of at the
// pixel (beam.glsl); its gradient is at most rad (0.48 / rc) px per px (rad in px, rc the half diagonal in px; 2 mm
// on a 19 in tube at 1440 lines: 0.004 px per px), an error well inside the T-48 contract.
bool geometry_separable() {
    return (params.geometry_curvature == GEOMETRY_CURVATURE_FLAT || !(params.geometry_radius_mm > 0.0))
        && params.geometry_pincushion == 0.0 && params.geometry_trapezoid == 0.0
        && params.geometry_rotation_deg == 0.0;
}

// Inverse of the vertical map of a separable geometry (geometry_separable): the host y (tube image) whose raster
// position is `ry` (host px from the raster's top-left). ry + raster_margin.y exactly when the geometry is the
// identity or the tube is not placed.
float geometry_separable_host_y(float ry) {
    float scale = geometry_raster_scale();
    float m = float(frame.raster_margin.y);
    if (geometry_identity(scale) || !(frame.px_per_mm > 0.0)) {
        return ry + m;
    }
    float c = 0.5 * float(frame.tube_size.y);
    return c + (ry + m - c) * ((1.0 + params.geometry_overscan_y) * scale);
}

// HOOK (beam_convergence_px): landing offset (host px) of the red, green and blue beams at host position p: the
// static offset per channel plus the radial term, radial_c (r / rc)^2 along the radius (rc the half diagonal of
// the picture), CRT.md Convergence (static offset plus a quadratic polynomial per channel). Zero by default.
// The offset is evaluated at the host position p where the light lands, not at the source point it comes from
// (the model's definition, crt_model::geometry::convergence_offset_mm, is at the source point): the landing
// point solves p = s + offset(p), the source-point form p = s + offset(s) is its first-order inverse. The
// difference is about 2 rad^2 r^3 / rc^4 (0.17 px at the slider maximum 2 mm in the corner of a 19 in tube at
// 1440 lines); tests/stage_geometry.rs asserts the landing-point value (G6).
void geometry_convergence_px(vec2 p, out vec3 dx, out vec3 dy) {
    vec3 sx = vec3(params.geometry_conv_red_dx_mm, params.geometry_conv_green_dx_mm, params.geometry_conv_blue_dx_mm);
    vec3 sy = vec3(params.geometry_conv_red_dy_mm, params.geometry_conv_green_dy_mm, params.geometry_conv_blue_dy_mm);
    vec3 rad = vec3(params.geometry_conv_red_radial_mm, params.geometry_conv_green_radial_mm,
        params.geometry_conv_blue_radial_mm);
    float ppm = frame.px_per_mm;
    if (!(ppm > 0.0) || (all(equal(sx, vec3(0.0))) && all(equal(sy, vec3(0.0))) && all(equal(rad, vec3(0.0))))) {
        dx = vec3(0.0);
        dy = vec3(0.0);
        return;
    }
    vec2 c = 0.5 * vec2(frame.tube_size);
    vec2 u = (p - c) / ppm;
    vec2 half_mm = 0.5 * vec2(geometry_raster_size()) / ppm;
    // u |u| / rc^2: the unit radial direction times (r / rc)^2.
    vec2 g = u * (length(u) / dot(half_mm, half_mm));
    dx = (sx + rad * g.x) * ppm;
    dy = (sy + rad * g.y) * ppm;
}

// Visible opening (T-37 review F1; CRT.md Visible area, overscan, corners: "a rounded rectangle defined by the
// tube's phosphor area and the cabinet bezel", corner radius 15 to 40 mm on older tubes, near zero on flat
// ones). The opening is the tube's visible area, tube_visible_height_mm x tube_aspect, centred in the tube image:
// the raster rectangle at zero overscan, widened to the tube's aspect when a `par` picture is narrower (or
// letterboxed, shorter) than the tube; with corners of radius
// geometry_opening_corner_radius_mm (clamped to the half height). The light is clipped to it, independently of
// the raster margin (the margin only sets the evaluated area; overscan, raster_scale and the spot no longer move
// the picture's outer edge). Coverage of host pixel q: clamp(0.5 - d, 0, 1), d the signed distance (host px,
// positive outside) from the pixel centre to the opening's outline: exactly 0 when the centre is half a pixel
// or more outside, exactly 1 half a pixel or more inside, a linear ramp over one host pixel across the outline
// (antialiasing; for a straight edge on pixel boundaries, which is the square opening of a placed tube, it is
// exactly 0 or 1). Exactly 1 everywhere while the tube is not placed.
float geometry_opening_coverage(ivec2 q) {
    float ppm = frame.px_per_mm;
    if (!(ppm > 0.0)) {
        return 1.0;
    }
    // The raster fills the visible area along one axis (placement.rs: the raster height is the visible height,
    // or its width the visible width when a `par` picture is wider than the tube); along the other the visible
    // area is the raster's size on that axis times (or over) tube_aspect when that is wider (taller) by more
    // than a pixel (a `par` picture narrower than the tube), else the raster's own, pixel-aligned size.
    vec2 rs = vec2(geometry_raster_size());
    vec2 open = rs;
    if (params.tube_aspect > 0.0) {
        vec2 vis = vec2(rs.y * params.tube_aspect, rs.x / params.tube_aspect);
        open = mix(rs, vis, greaterThan(vis, rs + 1.0));
    }
    vec2 half_px = 0.5 * open;
    vec2 p = abs(vec2(q) + 0.5 - 0.5 * vec2(frame.tube_size));
    float r = clamp(params.geometry_opening_corner_radius_mm * ppm, 0.0, min(half_px.x, half_px.y));
    vec2 e = p - (half_px - r);
    float d = length(max(e, vec2(0.0))) + min(max(e.x, e.y), 0.0) - r;
    return clamp(0.5 - d, 0.0, 1.0);
}

#endif
