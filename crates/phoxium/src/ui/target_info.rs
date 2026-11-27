//! What the Source panel shows under the `source_colour_variant` row: the name of the variant in force, one muted
//! line, for a target that has variants. Nothing else: the notes and the provenance of every value stay in the data
//! files (`presets/targets/*.toml`, [`crate::targets::Target`]) and in `docs/REFERENCE.md`; a menu is not a book.
//!
//! [`variant_name`] is pure (unit tested); [`draw`] puts it on the screen.

use dear_imgui_rs::Ui as Frame;

use crate::params::Params;
use crate::targets::Target;

use super::theme::{self, PALETTE};

/// The name of the variant in force for `variant`, the number of `source_colour_variant` (a number out of range uses
/// the first, [`Target::variant_index`]); `None` for a target without variants.
pub fn variant_name(target: &Target, variant: i32) -> Option<String> {
    target
        .variants
        .get(target.variant_index(variant))
        .map(|v| v.name.trim().to_string())
}

/// Draws the variant name under the variant row (nothing for `none` or a target without variants). Returns the
/// rectangle of the line by key (`colour_target:variant`), empty when nothing is drawn.
pub fn draw(ui: &Frame, params: &Params) -> Vec<(&'static str, [[f32; 2]; 2])> {
    let mut rects = Vec::new();
    let Some(target) = crate::targets::for_value(params.source_colour_target) else {
        return rects;
    };
    let Some(name) = variant_name(target, params.source_colour_variant) else {
        return rects;
    };
    let s = theme::scale_of(ui);
    let wrap = ui.push_text_wrap_pos(0.0);
    ui.text_colored(PALETTE.text_muted, &name);
    drop(wrap);
    rects.push(("colour_target:variant", [ui.item_rect_min(), ui.item_rect_max()]));
    ui.dummy([0.0, 2.0 * s]);
    rects
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A target with variants names the one in force: the numbered one, and the first for a number past the last or
    /// below 1.
    #[test]
    fn the_name_is_the_variant_in_force() {
        let t = crate::targets::by_id("nintendo_super_game_boy").unwrap();
        assert!(t.variants.len() > 9);
        for (number, index) in [
            (1, 0),
            (9, 8),
            (t.variants.len() as i32, t.variants.len() - 1),
            (0, 0),
            (99, 0),
        ] {
            assert_eq!(
                variant_name(t, number).as_deref(),
                Some(t.variants[index].name.trim()),
                "{number}"
            );
        }
    }

    /// Every shipped target gives a name exactly when it has variants, and the name is short enough for a menu line.
    #[test]
    fn only_a_target_with_variants_has_a_name() {
        for t in crate::targets::shipped() {
            let name = variant_name(t, 1);
            assert_eq!(name.is_some(), !t.variants.is_empty(), "{}", t.id);
            if let Some(n) = name {
                assert!(!n.is_empty() && n.chars().count() <= 90, "{}: {n}", t.id);
            }
        }
    }
}
