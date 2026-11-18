#version 450

// First pass of the pipeline: plain duplication of the source raster.
// The CRT model will replace this shader.
//
// The captured frame may be an integer upscale of the native source. The native pixel p is read at
// the centre of its block: origin + p * div + div / 2 (div = 1 when the source is native).
layout(set = 0, binding = 0) uniform sampler2D src;

layout(push_constant) uniform Push {
    vec4 rect;
    ivec2 origin;
    ivec2 div;
    ivec2 native;
} pc;

layout(location = 0) in vec2 uv;
layout(location = 0) out vec4 out_color;

void main() {
    ivec2 p = min(ivec2(uv * vec2(pc.native)), pc.native - 1);
    ivec2 texel = pc.origin + p * pc.div + pc.div / 2;
    out_color = vec4(texelFetch(src, texel, 0).rgb, 1.0);
}
