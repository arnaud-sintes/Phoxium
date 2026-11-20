// Interlace, manual field mode (T-32, QUESTIONS SRC-7; parameters params/interlace.toml; oracle
// crt_model::interlace, with the model and its conventions). Used by beam_raster (beam.glsl), which turns the
// progressive raster into the raster of the field shown in this output frame; nothing here runs while
// `interlace_mode` is off.
#ifndef UCRT_INTERLACE_GLSL
#define UCRT_INTERLACE_GLSL

// Specialization constant 0: the pipeline variant that can draw fields (src/gpu/stages/screen.rs creates it on
// first use, while `interlace_mode` is not off). The default variant (false) compiles the progressive raster alone,
// so with interlace off the driver's code is the code without this module and the picture is bit-identical.
layout(constant_id = 0) const bool UCRT_INTERLACE = false;

// The field shown in this output frame: 0 = top (upper lines, even rows of a woven frame), 1 = bottom. Locked by
// `interlace_field_select`, else the parity of the pipeline's output frame counter, inverted for
// bottom-field-first (crt_model::interlace::Interlace::field).
int interlace_field() {
    if (params.interlace_field_select == INTERLACE_FIELD_SELECT_TOP) {
        return 0;
    }
    if (params.interlace_field_select == INTERLACE_FIELD_SELECT_BOTTOM) {
        return 1;
    }
    uint f = frame.frame_index & 1u;
    if (params.interlace_field_order == INTERLACE_FIELD_ORDER_BOTTOM_FIRST) {
        f ^= 1u;
    }
    return int(f);
}

#endif
