//! "Saved profiles" in the menu's Source section (owner request 2026-11-20): the per-game profiles of
//! `settings.toml` (`[[source]]`, keyed by executable and window class) with their summary, Delete (confirmed) and
//! "Use for current source" (a copy for the program captured now), and "Reset this game's profile" for the
//! attached program. The list is `crate::profiles::rows`, given by the app in `super::UiState::profiles`; the
//! requests go back as `crate::profiles::ProfileAction`.

use dear_imgui_rs::Ui as Frame;

use crate::profiles::{ProfileAction, ProfileRow};

use super::menu::{record, same_line_if_fits, MenuResult};
use super::theme::{self, Fonts, METRICS, PALETTE};

/// The list's confirmations.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProfilesModel {
    /// A delete waiting for its confirmation: (executable, class).
    pub confirm_delete: Option<(String, String)>,
    /// "Reset this game's profile" waiting for its confirmation.
    pub confirm_reset: bool,
}

fn small(ui: &Frame, colour: theme::Rgba, text: &str) {
    theme::with_font(ui, None, METRICS.font_small, || {
        let wrap = ui.push_text_wrap_pos(0.0);
        ui.text_colored(colour, text);
        drop(wrap);
    });
}

/// The default profile (TODO Low): what a program without a profile of its own gets. Its summary, the button that
/// makes the profile of the program captured now the default, and the one that forgets it.
fn default_profile_rows(ui: &Frame, profiles: &[ProfileRow], default_profile: Option<&str>, r: &mut MenuResult) {
    ui.dummy([0.0, 2.0 * theme::scale_of(ui)]);
    ui.text_colored(PALETTE.text, "Default profile");
    record(ui, r, "profiles:default");
    match default_profile {
        Some(summary) => small(ui, PALETTE.text_muted, summary),
        None => small(
            ui,
            PALETTE.text_faint,
            "None: a program without a profile of its own uses the preset's source settings.",
        ),
    }
    let has_own = profiles.iter().any(|p| p.current);
    let disabled = ui.begin_disabled_with_cond(!has_own);
    if theme::button(ui, "Use this game's profile as the default") {
        r.profile = Some(ProfileAction::SetDefault);
    }
    drop(disabled);
    record(ui, r, "profiles:default_set");
    if ui.is_item_hovered() {
        theme::tooltip(
            ui,
            "Copy the saved profile of the program captured now (its scale mode, flip, filter and the other source \
             settings; its picture area and multiplier too) as the profile that every program without one gets. A \
             program that already has a profile keeps its own.",
        );
    }
    same_line_if_fits(ui, "Clear default");
    let disabled = ui.begin_disabled_with_cond(default_profile.is_none());
    if theme::button(ui, "Clear default") {
        r.profile = Some(ProfileAction::ClearDefault);
    }
    drop(disabled);
    record(ui, r, "profiles:default_clear");
    ui.dummy([0.0, 2.0 * theme::scale_of(ui)]);
}

/// The rows. `attached`: the program captured now (its executable, if known).
pub fn rows(
    ui: &Frame,
    fonts: &Fonts,
    m: &mut ProfilesModel,
    profiles: &[ProfileRow],
    default_profile: Option<&str>,
    attached: Option<&str>,
    r: &mut MenuResult,
) {
    theme::subheading(ui, fonts, "Saved profiles");
    if profiles.is_empty() {
        small(
            ui,
            PALETTE.text_faint,
            "No program has a profile yet. Changing the picture area or the multiplier saves one for the program \
             captured.",
        );
    }
    for (i, p) in profiles.iter().enumerate() {
        let key = format!("profile{i}");
        let _id = ui.push_id(&key);
        ui.text_colored(PALETTE.text, &p.exe);
        record(ui, r, &format!("profile:{i}"));
        ui.same_line();
        theme::with_font(ui, None, METRICS.font_small, || {
            ui.text_colored(PALETTE.text_faint, format!("\u{00B7}  {}", p.class));
            let tag = match (p.current, p.last_used) {
                (true, _) => Some(("current", PALETTE.accent)),
                (false, true) => Some(("last used", PALETTE.text_muted)),
                _ => None,
            };
            if let Some((t, c)) = tag {
                ui.same_line();
                theme::text_right(ui, c, t);
            }
        });
        small(ui, PALETTE.text_muted, &p.summary);
        let disabled = ui.begin_disabled_with_cond(attached.is_none() || p.current);
        if theme::button(ui, "Use for current source") {
            r.profile = Some(ProfileAction::UseForCurrent {
                exe: p.exe.clone(),
                class: p.class.clone(),
            });
        }
        drop(disabled);
        record(ui, r, &format!("profile:{i}:use"));
        if ui.is_item_hovered() {
            theme::tooltip(
                ui,
                "Copy this profile (picture area, multiplier, source settings) to the program captured now.",
            );
        }
        same_line_if_fits(ui, "Delete");
        if theme::button(ui, "Delete") {
            m.confirm_delete = Some((p.exe.clone(), p.class.clone()));
        }
        record(ui, r, &format!("profile:{i}:delete"));
        if m.confirm_delete.as_ref() == Some(&(p.exe.clone(), p.class.clone())) {
            theme::callout(
                ui,
                PALETTE.problem,
                &[format!(
                    "Delete the profile of {} ({})? Its picture area and multiplier are forgotten.",
                    p.exe, p.class
                )],
            );
            if theme::button_tinted(ui, "Delete profile", PALETTE.problem) {
                r.profile = Some(ProfileAction::Delete {
                    exe: p.exe.clone(),
                    class: p.class.clone(),
                });
                m.confirm_delete = None;
            }
            record(ui, r, &format!("profile:{i}:confirm"));
            ui.same_line();
            if theme::button(ui, "Cancel") {
                m.confirm_delete = None;
            }
            record(ui, r, &format!("profile:{i}:cancel"));
        }
        ui.dummy([0.0, 2.0 * theme::scale_of(ui)]);
    }
    default_profile_rows(ui, profiles, default_profile, r);
    let Some(exe) = attached else {
        return;
    };
    let has = profiles.iter().any(|p| p.current);
    let disabled = ui.begin_disabled_with_cond(!has);
    if theme::button(ui, "Reset this game's profile") {
        m.confirm_reset = true;
    }
    drop(disabled);
    record(ui, r, "profiles:reset_current");
    if ui.is_item_hovered() {
        theme::tooltip(
            ui,
            "Forget what was saved for the program captured now: the picture area goes back to Auto and the source \
             settings to the preset's.",
        );
    }
    if m.confirm_reset && has {
        theme::callout(
            ui,
            PALETTE.problem,
            &[format!(
                "Reset the profile of {exe}? The picture area goes back to Auto."
            )],
        );
        if theme::button_tinted(ui, "Reset profile", PALETTE.problem) {
            r.profile = Some(ProfileAction::ResetCurrent);
            m.confirm_reset = false;
        }
        record(ui, r, "profiles:reset_confirm");
        ui.same_line();
        if theme::button(ui, "Cancel##reset_profile") {
            m.confirm_reset = false;
        }
        record(ui, r, "profiles:reset_cancel");
    }
    ui.dummy([0.0, 4.0 * theme::scale_of(ui)]);
}
