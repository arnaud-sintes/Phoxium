//! The details of the chosen colour target in the Source and placement panel, under the `source_colour_variant`
//! row: the data file's one-line label with the target and variant names, its notes and the provenance of every value
//! (the tag S, D, K or E and the source text), the last two collapsed by default.
//!
//! Every word comes from the data file (`presets/targets/*.toml`, [`crate::targets::Target`]); this module only lays
//! it out. [`target_info`] is pure (the layout logic, unit tested); [`draw`] puts it on the screen.

use dear_imgui_rs::Ui as Frame;

use crate::params::Params;
use crate::targets::Target;

use super::theme::{self, PALETTE};

/// The tags of a provenance entry, in the order the legend and the counts give them.
pub const TAGS: [char; 4] = ['S', 'D', 'K', 'E'];

/// What the four tags mean (the confidence tags of `docs/DESIGN.md`).
pub const LEGEND: &str = "S sourced, D derived, K standard knowledge, E estimate";

/// One value of the target and where it comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProvenanceLine {
    /// The key of the data file's table (`rows`, `output`).
    pub key: String,
    /// `S`, `D`, `K` or `E`.
    pub tag: char,
    /// The source, as the data file words it.
    pub text: String,
}

/// The lines the menu shows for one target and variant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetInfo {
    /// The target's name, followed by the name of the variant in force when the target has variants.
    pub heading: String,
    /// The data file's one-line label.
    pub label: String,
    /// The notes, one entry per line of the data file's text (the files write one paragraph per line).
    pub notes: Vec<String>,
    /// The title of the provenance section: its entry count by tag, `Provenance (S 2, D 1, E 3)`.
    pub provenance_title: String,
    /// One line per provenance entry, in the key order of the data file's table.
    pub provenance: Vec<ProvenanceLine>,
}

/// The lines for `target` with `variant` the number of `source_colour_variant` (the variant in force is
/// [`Target::variant_index`]: a number out of range uses the first).
pub fn target_info(target: &Target, variant: i32) -> TargetInfo {
    let mut heading = target.name.trim().to_string();
    if let Some(v) = target.variants.get(target.variant_index(variant)) {
        heading.push_str(" \u{00B7} ");
        heading.push_str(v.name.trim());
    }
    let notes = target
        .notes
        .lines()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect();
    let provenance: Vec<ProvenanceLine> = target
        .provenance
        .iter()
        .map(|(key, entry)| {
            // `Target::parse` checks the form "<tag>: <source>" of every entry.
            let (tag, text) = entry.split_once(": ").unwrap_or(("?", entry));
            ProvenanceLine {
                key: key.clone(),
                tag: tag.chars().next().unwrap_or('?'),
                text: text.trim().to_string(),
            }
        })
        .collect();
    let counts: Vec<String> = TAGS
        .iter()
        .filter_map(|t| {
            let n = provenance.iter().filter(|p| p.tag == *t).count();
            (n > 0).then(|| format!("{t} {n}"))
        })
        .collect();
    TargetInfo {
        heading,
        label: target.label.trim().to_string(),
        notes,
        provenance_title: format!("Provenance ({})", counts.join(", ")),
        provenance,
    }
}

/// Draws the details of the target `params` names (nothing for `none`): the heading and label always, the notes
/// and the provenance behind two collapsed headers. Returns the rectangles of the two headers by key
/// (`colour_target:notes`, `colour_target:provenance`; empty for `none`).
pub fn draw(ui: &Frame, params: &Params) -> Vec<(&'static str, [[f32; 2]; 2])> {
    let mut rects = Vec::new();
    let Some(target) = crate::targets::for_value(params.source_colour_target) else {
        return rects;
    };
    let info = target_info(target, params.source_colour_variant);
    let s = theme::scale_of(ui);
    ui.dummy([0.0, 2.0 * s]);
    let wrap = ui.push_text_wrap_pos(0.0);
    ui.text_colored(PALETTE.text, &info.heading);
    ui.text_colored(PALETTE.text_muted, &info.label);
    drop(wrap);
    let node = ui.tree_node_config("colour_target_notes").label("Notes").push();
    rects.push(("colour_target:notes", [ui.item_rect_min(), ui.item_rect_max()]));
    if let Some(_node) = node {
        let wrap = ui.push_text_wrap_pos(0.0);
        for (i, paragraph) in info.notes.iter().enumerate() {
            if i > 0 {
                ui.dummy([0.0, 4.0 * s]);
            }
            ui.text_colored(PALETTE.text_muted, paragraph);
        }
        drop(wrap);
    }
    let node = ui
        .tree_node_config("colour_target_provenance")
        .label(&info.provenance_title)
        .push();
    rects.push(("colour_target:provenance", [ui.item_rect_min(), ui.item_rect_max()]));
    if let Some(_node) = node {
        let wrap = ui.push_text_wrap_pos(0.0);
        ui.text_colored(PALETTE.text_faint, LEGEND);
        for line in &info.provenance {
            ui.dummy([0.0, 2.0 * s]);
            ui.text_colored(PALETTE.accent, format!("[{}]", line.tag));
            ui.same_line();
            ui.text_colored(PALETTE.text, &line.key);
            ui.text_colored(PALETTE.text_muted, &line.text);
        }
        drop(wrap);
    }
    ui.dummy([0.0, 2.0 * s]);
    rects
}

#[cfg(test)]
mod tests {
    use super::*;

    fn squeeze(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// Every shipped target gives a heading with its name, its label, all of its notes (nothing lost by the paragraph
    /// split) and one line per provenance entry with a valid tag and the data file's own text.
    #[test]
    fn every_shipped_target_lays_out_from_its_data_file() {
        for t in crate::targets::shipped() {
            let info = target_info(t, 1);
            assert!(info.heading.starts_with(t.name.trim()), "{}", t.id);
            assert_eq!(info.label, t.label.trim(), "{}", t.id);
            assert!(
                !info.notes.is_empty() && info.notes.iter().all(|p| !p.is_empty()),
                "{}",
                t.id
            );
            assert_eq!(squeeze(&info.notes.join(" ")), squeeze(&t.notes), "{}: notes", t.id);
            assert_eq!(info.provenance.len(), t.provenance.len(), "{}", t.id);
            for (line, (key, entry)) in info.provenance.iter().zip(&t.provenance) {
                assert_eq!(&line.key, key);
                assert!(TAGS.contains(&line.tag), "{}: {key}", t.id);
                assert_eq!(entry, &format!("{}: {}", line.tag, line.text), "{}: {key}", t.id);
            }
            // The counts of the title add up to the number of entries.
            let counted: usize = info
                .provenance_title
                .trim_start_matches("Provenance (")
                .trim_end_matches(')')
                .split(", ")
                .map(|c| c.split_once(' ').unwrap().1.parse::<usize>().unwrap())
                .sum();
            assert_eq!(counted, t.provenance.len(), "{}: {}", t.id, info.provenance_title);
            assert_eq!(t.variants.is_empty(), !info.heading.contains(" \u{00B7} "), "{}", t.id);
        }
    }

    /// The heading names the variant in force: the numbered one, and the first for a number past the last or below 1.
    #[test]
    fn the_heading_names_the_variant_in_force() {
        let t = crate::targets::by_id("super_game_boy").unwrap();
        assert!(t.variants.len() > 9);
        for (number, index) in [
            (1, 0),
            (9, 8),
            (t.variants.len() as i32, t.variants.len() - 1),
            (0, 0),
            (99, 0),
        ] {
            let info = target_info(t, number);
            let name = &t.variants[index].name;
            assert!(
                info.heading.ends_with(&format!(" \u{00B7} {}", name.trim())),
                "{number}: {}",
                info.heading
            );
        }
    }

    /// Paragraphs are the non-blank lines, trimmed; a made-up target shows the counting and the order of the tags.
    #[test]
    fn paragraphs_and_tag_counts() {
        let mut t = crate::targets::by_id("game_boy_color").unwrap().clone();
        t.notes = "\n  First line\nsecond line.\n\n\n\nSecond paragraph.  \n".into();
        t.provenance.clear();
        for (k, v) in [("a", "E: x"), ("b", "S: y"), ("c", "E: z"), ("d", "D: w")] {
            t.provenance.insert(k.into(), v.into());
        }
        let info = target_info(&t, 1);
        assert_eq!(info.notes, ["First line", "second line.", "Second paragraph."]);
        assert_eq!(info.provenance_title, "Provenance (S 1, D 1, E 2)");
        assert_eq!(
            info.provenance[1],
            ProvenanceLine {
                key: "b".into(),
                tag: 'S',
                text: "y".into()
            }
        );
        assert_eq!(LEGEND.matches(", ").count(), TAGS.len() - 1);
    }
}
