#version 450

// Draws a textured quad as a 4-vertex triangle strip, no vertex buffer.
// rect is in NDC: xy = top-left corner, zw = bottom-right corner (letterboxing).
layout(push_constant) uniform Push {
    vec4 rect;
    ivec2 origin;
    ivec2 div;
    ivec2 native;
} pc;

layout(location = 0) out vec2 uv;

void main() {
    vec2 c = vec2(float(gl_VertexIndex & 1), float((gl_VertexIndex >> 1) & 1));
    uv = c;
    gl_Position = vec4(mix(pc.rect.xy, pc.rect.zw, c), 0.0, 1.0);
}
