//! The per-game profiles as the menu's Source section lists them ("Saved profiles", owner request 2026-11-20):
//! the `[[source]]` entries of `settings.toml`, keyed by executable and window class, each with a one-line
//! summary of its picture area and scaling multiplier. Pure; the app applies the requests (delete, use for the
//! current source, reset the attached program's profile).

use crate::settings::{Settings, SourceProfile};

/// One saved profile in the list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProfileRow {
    pub exe: String,
    pub class: String,
    /// `crop 768 × 672 at 73, 41  ·  x3  ·  +2 more`.
    pub summary: String,
    /// The program and class chosen last (`[global.source] last`): the only use the settings remember.
    pub last_used: bool,
    /// The program and class captured now.
    pub current: bool,
}

/// A request of the "Saved profiles" list, for the app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProfileAction {
    /// Remove this profile (confirmed in the menu).
    Delete { exe: String, class: String },
    /// Copy this profile to the program and class captured now, and apply it.
    UseForCurrent { exe: String, class: String },
    /// Remove the profile of the program captured now and go back to the preset's source settings.
    ResetCurrent,
    /// Make the profile of the program captured now the default profile, the one a program without a profile of
    /// its own gets (`[default_source]`).
    SetDefault,
    /// Forget the default profile.
    ClearDefault,
}

fn eq(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// The picture area and multiplier of a profile in words, with the count of its other settings.
pub fn summary(p: &SourceProfile) -> String {
    let mode = |v: &Option<String>| v.as_deref().map(str::to_ascii_lowercase);
    let area = match (
        mode(&p.crop_mode).as_deref(),
        mode(&p.picture_area).as_deref(),
        p.crop_rect(),
    ) {
        (Some("none"), _, _) => "whole window".to_string(),
        (_, Some("client"), _) => "whole client area".to_string(),
        (Some("manual"), _, Some(r)) | (_, Some("manual"), Some(r)) => {
            format!("crop {} \u{00D7} {} at {}, {}", r.w, r.h, r.x, r.y)
        }
        _ => "picture auto".to_string(),
    };
    let multiplier = match (mode(&p.divisor_mode).as_deref(), p.divisor_pair()) {
        (Some("none"), _) => "native (x1)".to_string(),
        (Some("manual"), Some(d)) => crate::scaling::Multiplier::pair_text(d),
        _ => "multiplier auto".to_string(),
    };
    let more = [
        p.par.is_some(),
        p.eotf.is_some(),
        p.pixel_clock_hz.is_some(),
        p.bandwidth_hz.is_some(),
        p.line_multiplier.is_some(),
        p.scale_mode.is_some(),
        p.scale.is_some(),
        p.center.is_some(),
        p.surround_color.is_some(),
        p.display_aspect.is_some(),
        p.raster_aspect.is_some(),
        p.rotation.is_some(),
        p.flip.is_some(),
        p.prefilter.is_some(),
        p.colour_target.is_some(),
        p.pixel_size.is_some(),
    ]
    .iter()
    .filter(|b| **b)
    .count();
    let mut s = format!("{area}  \u{00B7}  {multiplier}");
    if more > 0 {
        s.push_str(&format!("  \u{00B7}  +{more} more"));
    }
    s
}

/// The rows of the list, in file order; `current` is the program and class captured now (executable, class).
pub fn rows(settings: &Settings, current: Option<(&str, &str)>) -> Vec<ProfileRow> {
    let last = settings.global.source.last.as_ref();
    settings
        .sources
        .iter()
        .map(|p| ProfileRow {
            exe: p.exe.clone(),
            class: p.class.clone(),
            summary: summary(p),
            last_used: last.is_some_and(|l| eq(&l.exe, &p.exe) && eq(&l.class, &p.class)),
            current: current.is_some_and(|(e, c)| eq(e, &p.exe) && eq(c, &p.class)),
        })
        .collect()
}

/// The default profile as a line of the list (`None` without one): the same summary as a program's profile.
pub fn default_summary(settings: &Settings) -> Option<String> {
    settings.default_source.as_ref().map(summary)
}

/// The default profile made of the profile of a program: its source settings (scale mode and size, flip, filter,
/// aspect, rotation, colour interpretation, surround) with no executable and class. What belongs to one program's
/// window is left out: the crop rectangle and the client size it was made for, the divisor and the picture area (a
/// manual crop of one game would cut every other game's picture), and the crop and divisor modes.
pub fn as_default(from: &SourceProfile) -> SourceProfile {
    SourceProfile {
        exe: String::new(),
        class: String::new(),
        crop: None,
        crop_client: None,
        divisor: None,
        divisor_v: None,
        crop_mode: None,
        divisor_mode: None,
        picture_area: None,
        ..from.clone()
    }
}

/// The profile `from` for the program and class `exe`, `class` ("Use for current source").
pub fn copy_to(from: &SourceProfile, exe: &str, class: &str) -> SourceProfile {
    SourceProfile {
        exe: exe.to_string(),
        class: class.to_string(),
        ..from.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::LastSource;

    fn profile(exe: &str, class: &str) -> SourceProfile {
        SourceProfile {
            exe: exe.into(),
            class: class.into(),
            ..SourceProfile::default()
        }
    }

    /// The default profile keeps the source settings and drops what belongs to one program's window (its crop,
    /// divisor and picture area), so a game's manual crop is not handed to every other game.
    #[test]
    fn the_default_profile_drops_the_window_specific_settings() {
        let own = SourceProfile {
            crop: Some([73, 41, 841, 713]),
            crop_client: Some([914, 754]),
            crop_mode: Some("manual".into()),
            picture_area: Some("manual".into()),
            divisor: Some(3),
            divisor_v: Some(2),
            divisor_mode: Some("manual".into()),
            scale_mode: Some("manual".into()),
            pixel_size: Some(2.5),
            flip: Some("horizontal".into()),
            prefilter: Some("box".into()),
            par: Some(8.0 / 7.0),
            rotation: Some("clockwise".into()),
            ..profile("game.exe", "GameClass")
        };
        let d = as_default(&own);
        assert_eq!((d.exe.as_str(), d.class.as_str()), ("", ""));
        assert_eq!(
            (
                d.crop,
                d.crop_client,
                d.divisor,
                d.divisor_v,
                d.crop_mode,
                d.divisor_mode,
                d.picture_area
            ),
            (None, None, None, None, None, None, None)
        );
        assert_eq!(
            (
                d.scale_mode.as_deref(),
                d.pixel_size,
                d.flip.as_deref(),
                d.prefilter.as_deref()
            ),
            (Some("manual"), Some(2.5), Some("horizontal"), Some("box"))
        );
        assert_eq!((d.par, d.rotation.as_deref()), (Some(8.0 / 7.0), Some("clockwise")));
    }

    #[test]
    fn summaries_name_the_area_and_the_multiplier() {
        assert_eq!(
            summary(&profile("a.exe", "A")),
            "picture auto  \u{00B7}  multiplier auto"
        );
        let manual = SourceProfile {
            crop: Some([73, 41, 841, 713]),
            crop_mode: Some("manual".into()),
            picture_area: Some("manual".into()),
            divisor_mode: Some("manual".into()),
            divisor: Some(3),
            par: Some(1.0),
            center: Some(true),
            ..profile("a.exe", "A")
        };
        assert_eq!(
            summary(&manual),
            "crop 768 \u{00D7} 672 at 73, 41  \u{00B7}  x3  \u{00B7}  +2 more"
        );
        let client = SourceProfile {
            picture_area: Some("client".into()),
            divisor_mode: Some("none".into()),
            ..profile("a.exe", "A")
        };
        assert_eq!(summary(&client), "whole client area  \u{00B7}  native (x1)");
        let window = SourceProfile {
            crop_mode: Some("none".into()),
            ..profile("a.exe", "A")
        };
        assert!(summary(&window).starts_with("whole window"));
    }

    #[test]
    fn rows_mark_the_last_used_and_the_current_program() {
        let mut s = Settings::default();
        s.upsert_source(profile("snes9x.exe", "Snes9X"));
        s.upsert_source(profile("game.exe", "Unity"));
        s.global.source.last = Some(LastSource {
            exe: "GAME.EXE".into(),
            class: "unity".into(),
            title: "t".into(),
        });
        let r = rows(&s, Some(("snes9x.exe", "snes9x")));
        assert_eq!(r.len(), 2);
        assert_eq!(
            (r[0].exe.as_str(), r[0].current, r[0].last_used),
            ("snes9x.exe", true, false)
        );
        assert_eq!(
            (r[1].exe.as_str(), r[1].current, r[1].last_used),
            ("game.exe", false, true)
        );
        assert!(rows(&Settings::default(), None).is_empty());
    }

    #[test]
    fn a_profile_is_copied_to_another_program() {
        let from = SourceProfile {
            divisor: Some(4),
            ..profile("a.exe", "A")
        };
        let c = copy_to(&from, "b.exe", "B");
        assert_eq!((c.exe.as_str(), c.class.as_str(), c.divisor), ("b.exe", "B", Some(4)));
    }
}
