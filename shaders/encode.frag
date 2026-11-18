#version 450
#extension GL_GOOGLE_include_directive : require

// Stage `encode` (ARCHITECTURE 3.7): linear light -> output target (swapchain or headless image).
//
// Inside the tube rectangle: the linear value of the native pixel under the fragment (nearest, T-20a has
// no screen stage yet), times `exposure`, clipped to [0, 1] (panel white) and encoded with the inverse of
// the host display EOTF chosen by the push constants. Outside: black surround. No dither (T-46).

#include "params.glsl"
#include "common.glsl"

layout(set = 0, binding = 2) uniform sampler2D linear_in;

layout(push_constant) uniform Push {
    uint transfer;
    float gamma;
} pc;

layout(location = 0) out vec4 out_color;

void main() {
    vec2 f = gl_FragCoord.xy;
    vec4 r = frame.tube_rect_px;
    if (f.x < r.x || f.y < r.y || f.x >= r.z || f.y >= r.w) {
        out_color = vec4(0.0, 0.0, 0.0, 1.0);
        return;
    }
    vec2 uv = (f - r.xy) / (r.zw - r.xy);
    ivec2 p = min(ivec2(uv * vec2(frame.native_size)), frame.native_size - 1);
    vec3 l = texelFetch(linear_in, p, 0).rgb * params.exposure;
    out_color = vec4(transfer_from_linear(clamp(l, 0.0, 1.0), pc.transfer, pc.gamma), 1.0);
}
