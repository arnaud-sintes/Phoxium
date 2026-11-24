// Cabinet bezel and its reflection in the glass (params/cabinet.toml), used by `encode.frag`. The f64 oracle is
// crates/crt-model/src/cabinet.rs, whose module documentation holds the model; keep both in step.
#ifndef PHOXIUM_CABINET_GLSL
#define PHOXIUM_CABINET_GLSL

const float CABINET_AMBIENT = 0.35;
const float CABINET_ROLL_START = 0.85;

// The band in whole pixels: a share of the output's shorter side, rounded half up (`cabinet::inset_px`).
float cabinet_band(float width, ivec2 target) {
    return max(floor(width * float(min(target.x, target.y)) + 0.5), 0.0);
}

float cabinet_srgb_eotf(float v) {
    v = clamp(v, 0.0, 1.0);
    return v <= 0.04045 ? v / 12.92 : pow((v + 0.055) / 1.055, 2.4);
}

vec3 cabinet_face_linear(vec3 code) {
    return vec3(cabinet_srgb_eotf(code.r), cabinet_srgb_eotf(code.g), cabinet_srgb_eotf(code.b));
}

// The shade (1 on the flat face) at distance `dist` from the picture rectangle, outward direction `n`.
float cabinet_shade(float dist, vec2 n, float band, float bevel) {
    float t = dist / band;
    const float s = 0.70710678118654752;
    vec3 normal = t < bevel ? vec3(-n.x * s, -n.y * s, s) : vec3(0.0, 0.0, 1.0);
    vec3 l = normalize(vec3(-0.45, -0.55, 0.70));
    float face = CABINET_AMBIENT + (1.0 - CABINET_AMBIENT) * max(l.z, 0.0);
    float lit = CABINET_AMBIENT + (1.0 - CABINET_AMBIENT) * max(dot(normal, l), 0.0);
    return lit / face * (1.0 - 0.5 * smoothstep(CABINET_ROLL_START, 1.0, t));
}

// The bezel at the pixel centre `p` for the picture rectangle `rect` (x0, y0, x1, y1): (linear colour, coverage),
// coverage 0 inside the rectangle and outside the rounded outer shape.
vec4 cabinet_bezel(vec2 p, vec4 rect, float band, vec3 colour, float bevel, float corner) {
    vec2 nearest = clamp(p, rect.xy, rect.zw);
    vec2 d = p - nearest;
    float dist = length(d);
    if (band < 1.0 || dist == 0.0) {
        return vec4(0.0);
    }
    float radius = corner * band;
    vec2 centre = 0.5 * (rect.xy + rect.zw);
    vec2 half_size = 0.5 * (rect.zw - rect.xy) + band;
    vec2 q = abs(p - centre) - (half_size - radius);
    float sd = length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - radius;
    float coverage = clamp(0.5 - sd, 0.0, 1.0);
    if (coverage == 0.0) {
        return vec4(0.0);
    }
    float shade = cabinet_shade(dist, d / dist, band, bevel);
    return vec4(cabinet_face_linear(colour) * shade, coverage);
}

// The linear light the glass adds at the pixel centre `p` inside `rect`: a faint mirror image of the bezel,
// fading with the distance to the nearest edge (ties: left, right, top, bottom).
vec3 cabinet_reflection(vec2 p, vec4 rect, float band, vec3 colour, float bevel, float reflectivity) {
    if (band < 1.0 || reflectivity <= 0.0) {
        return vec3(0.0);
    }
    if (p.x < rect.x || p.x > rect.z || p.y < rect.y || p.y > rect.w) {
        return vec3(0.0);
    }
    float d[4] = float[4](p.x - rect.x, rect.z - p.x, p.y - rect.y, rect.w - p.y);
    vec2 normals[4] = vec2[4](vec2(-1.0, 0.0), vec2(1.0, 0.0), vec2(0.0, -1.0), vec2(0.0, 1.0));
    int k = 0;
    for (int i = 1; i < 4; i++) {
        if (d[i] < d[k]) {
            k = i;
        }
    }
    if (d[k] >= band) {
        return vec3(0.0);
    }
    float t = d[k] / band;
    return cabinet_face_linear(colour) * cabinet_shade(d[k], normals[k], band, bevel) * reflectivity * (1.0 - t);
}

#endif
