//! Interlace, manual field mode (T-32, QUESTIONS SRC-7): the parameters of `params/interlace.toml` as the
//! `crt-model` oracle's settings ([`model`]), and the refresh notice of the live loop ([`notice`]). The field
//! itself is chosen on the GPU (`shaders/include/interlace.glsl`) from the pipeline's output frame counter, and
//! the raster of that field is drawn by `beam_raster` (`shaders/include/beam.glsl`); off by default, where the
//! progressive raster is drawn unchanged.

use crt_model::interlace::{rate_notice, FieldMode, FieldOrder, FieldSelect, Interlace};

use crate::params::{
    Params, INTERLACE_FIELD_ORDER_BOTTOM_FIRST, INTERLACE_FIELD_SELECT_BOTTOM, INTERLACE_FIELD_SELECT_TOP,
    INTERLACE_MODE_ALTERNATE_FRAMES_ARE_FIELDS, INTERLACE_MODE_SPLIT_WOVEN_FRAME,
};

/// The interlace settings of `params` (an unknown enum value reads as the default).
pub fn model(params: &Params) -> Interlace {
    Interlace {
        mode: match params.interlace_mode {
            INTERLACE_MODE_ALTERNATE_FRAMES_ARE_FIELDS => FieldMode::AlternateFramesAreFields,
            INTERLACE_MODE_SPLIT_WOVEN_FRAME => FieldMode::SplitWovenFrame,
            _ => FieldMode::Off,
        },
        order: if params.interlace_field_order == INTERLACE_FIELD_ORDER_BOTTOM_FIRST {
            FieldOrder::BottomFirst
        } else {
            FieldOrder::TopFirst
        },
        select: match params.interlace_field_select {
            INTERLACE_FIELD_SELECT_TOP => FieldSelect::Top,
            INTERLACE_FIELD_SELECT_BOTTOM => FieldSelect::Bottom,
            _ => FieldSelect::Alternate,
        },
    }
}

/// The notice of the live loop when the output refresh does not match the field rate (PLAN T-32), from the
/// rates measured over the last second; `None` when interlace is off, a field is locked or the rates match.
pub fn notice(params: &Params, capture_hz: f64, output_hz: f64) -> Option<String> {
    let il = model(params);
    if il.select != FieldSelect::Alternate {
        return None;
    }
    rate_notice(il.mode, capture_hz, output_hz)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_off() {
        assert_eq!(model(&Params::default()), Interlace::OFF);
        assert_eq!(notice(&Params::default(), 30.0, 144.0), None);
    }

    #[test]
    fn enums_map_to_the_oracle() {
        let p = Params {
            interlace_mode: INTERLACE_MODE_SPLIT_WOVEN_FRAME,
            interlace_field_order: INTERLACE_FIELD_ORDER_BOTTOM_FIRST,
            interlace_field_select: INTERLACE_FIELD_SELECT_BOTTOM,
            ..Params::default()
        };
        let il = model(&p);
        assert_eq!(il.mode, FieldMode::SplitWovenFrame);
        assert_eq!(il.order, FieldOrder::BottomFirst);
        assert_eq!(il.select, FieldSelect::Bottom);
        // A locked field is a still picture: no refresh notice.
        assert_eq!(notice(&p, 30.0, 144.0), None);
        let alt = Params {
            interlace_mode: INTERLACE_MODE_ALTERNATE_FRAMES_ARE_FIELDS,
            ..Params::default()
        };
        assert!(notice(&alt, 30.0, 60.0).is_some());
        assert_eq!(notice(&alt, 60.0, 60.0), None);
    }
}
