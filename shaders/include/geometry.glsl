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
//    three fixed-point steps (contraction 2|k| <= 0.1, residual error below (2k)^3 of the displacement).
// 5. size: the nominal raster is scaled by (1 + overscan) per axis and by raster_scale (ABL breathing, T-35)
//    about the tube centre, so the raster point is the landing point divided by those factors.
// Identity (every parameter at its default, raster_scale 1): geometry_footprint returns vec2(q) exactly through
// a uniform branch (no cost, bit-identical to the T-36b render).
//
// Footprint. The beam is integrated over the host pixel's footprint moved to the raster position of its centre,
// with its size unchanged (1 x 1 raster px): the surface is Lambertian, so a compressed area keeps its luminance
// (a curved edge shows less raster, as bright). The local stretch (at most 1 / cos of the corner angle, 1.6 % for
// a 1R corner) is not applied to the footprint.
#ifndef UCRT_GEOMETRY_GLSL
#define UCRT_GEOMETRY_GLSL

const float GEOMETRY_DEG = 0.017453292;

// raster_scale = 1 + abl_breathing (apl_f - abl_breathing_ref), read from the ABL state the `abl` slot left for
// this frame (its reduce pass runs before `screen`; src/gpu/stages/screen.rs binds that buffer as set 1).
// Exactly 1 when breathing is off, without reading the buffer. A value out of [0.5, 2] (a state never written,
// when the abl slot does not run) is ignored.
float geometry_raster_scale() {
    if (params.abl_breathing == 0.0) {
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
    vec2 half_mm = c / ppm;
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
        for (int i = 0; i < 3; i++) {
            s = land / (1.0 + k * dot(s, s) / rc2);
        }
    }
    // 5. Size: overscan per axis and raster_scale.
    s /= vec2(1.0 + params.geometry_overscan_x, 1.0 + params.geometry_overscan_y) * scale;
    return c + s * ppm;
}

// HOOK (beam_eval): origin of the footprint of host pixel q in the beam raster (host px). vec2(q) exactly when
// the geometry is the identity or the tube is not placed.
vec2 geometry_footprint(ivec2 q) {
    float scale = geometry_raster_scale();
    if (geometry_identity(scale) || !(frame.px_per_mm > 0.0)) {
        return vec2(q);
    }
    return geometry_raster_px(vec2(q) + 0.5, scale) - 0.5;
}

// HOOK (beam_convergence_px): landing offset (host px) of the red, green and blue beams at host position p: the
// static offset per channel plus the radial term, radial_c (r / rc)^2 along the radius (rc the half diagonal of
// the picture), CRT.md Convergence (static offset plus a quadratic polynomial per channel). Zero by default.
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
    vec2 half_mm = c / ppm;
    // u |u| / rc^2: the unit radial direction times (r / rc)^2.
    vec2 g = u * (length(u) / dot(half_mm, half_mm));
    dx = (sx + rad * g.x) * ppm;
    dy = (sy + rad * g.y) * ppm;
}

#endif
