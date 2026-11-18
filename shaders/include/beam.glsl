// Beam profile (ARCHITECTURE 3.7, `screen` slot): beam current per channel deposited on host pixel q of
// the tube rectangle. T-36a (vertical scanline profile, area-integrated) and T-36b (horizontal spot,
// spot growth, convergence) replace the stub.
//
// Inputs: `current` is the gun output (amp domain, amp_size(), beam current per channel); `q` is the
// host pixel index in the tube image (0 .. frame.tube_size - 1), whose footprint is [q, q + 1) in
// tube-image pixels, origin at the tube rectangle's top-left; lengths in tube mm use frame.px_per_mm and
// frame.line_pitch_mm.
#ifndef UCRT_BEAM_GLSL
#define UCRT_BEAM_GLSL

// Stub (T-20b): the nearest amp sample under the pixel centre, an exact identity of the gun output.
vec3 beam_eval(sampler2D current, ivec2 q) {
    ivec2 n = amp_size();
    ivec2 s = ivec2(floor((vec2(q) + 0.5) * vec2(n) / vec2(frame.tube_size)));
    return texelFetch(current, min(s, n - 1), 0).rgb;
}

#endif
