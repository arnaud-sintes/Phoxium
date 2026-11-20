//! The preset files tools of the menu's Preset section ("Manage files", owner request 2026-11-20): rename, delete (with a confirmation),
//! duplicate and export a preset, and import preset files, through `crate::preset_files`. The folder-based paths
//! are the main ones: export writes to `%APPDATA%\ultimateCRT\export\`, import lists the `.toml` files dropped in
//! `%APPDATA%\ultimateCRT\import\` (Rescan). The native Save and Open dialogs are optional buttons
//! ([`DialogRequest`]): the app opens them between two frames (`crate::shell`) and hands the path back
//! (`super::Ui::dialog_result`). Shipped presets are never written or removed.

use std::path::{Path, PathBuf};

use dear_imgui_rs::Ui as Frame;

use crate::preset_files::{self as files, Conflict, ImportMode, Prepared};
use crate::presets::{Library, Origin};

use super::menu::{record, same_line_if_fits, MenuResult};
use super::theme::{self, Fonts, METRICS, PALETTE};

/// A native dialog the app opens for the menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DialogRequest {
    /// "Export to file...": a Save dialog for the preset of this id.
    Export(String),
    /// "Import file...": an Open dialog.
    Import,
}

/// What a dialog gave (`None`: cancelled).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DialogResult {
    Export { id: String, path: Option<PathBuf> },
    Import(Option<PathBuf>),
}

/// The section's state.
#[derive(Clone, Debug, Default)]
pub struct FilesModel {
    /// The tools are shown ("Manage files" in the Preset section).
    pub open: bool,
    /// The preset the tools act on; `None`: the preset in force.
    pub selected: Option<String>,
    /// The new name typed for Rename.
    pub rename: String,
    /// A delete waiting for its confirmation (the preset id).
    pub confirm_delete: Option<String>,
    /// An import whose id a user preset already has, waiting for Replace, Import a copy or Cancel.
    pub pending: Option<Prepared>,
    /// The `.toml` files of the import folder, from the last Rescan (`None`: not scanned yet).
    pub import_rows: Option<Vec<(PathBuf, u64)>>,
    /// The outcome of the last action: (good news, text).
    pub message: Option<(bool, String)>,
    pub export_dir: Option<PathBuf>,
    pub import_dir: Option<PathBuf>,
    /// Offer the native dialog buttons (the live app; headless tests may turn them on).
    pub dialogs: bool,
}

/// Reads and installs the preset file `path` (a row of the import folder or an Open dialog). Returns the new
/// library when a preset was written.
pub fn import_path(m: &mut FilesModel, lib: &Library, dir: &Path, path: &Path) -> Option<Library> {
    match files::read_import(path, lib) {
        Err(e) => {
            m.message = Some((false, e));
            None
        }
        Ok(prep) if prep.conflict == Conflict::User => {
            // The confirmation callout says it; no second message.
            m.message = None;
            m.pending = Some(prep);
            None
        }
        Ok(prep) => install(m, lib, dir, &prep, ImportMode::New),
    }
}

/// Installs a prepared import and says how it went.
fn install(m: &mut FilesModel, lib: &Library, dir: &Path, prep: &Prepared, mode: ImportMode) -> Option<Library> {
    match files::install(prep, lib, dir, mode) {
        Ok(id) => {
            let adjusted = prep.warnings.len();
            let note = match adjusted {
                0 => String::new(),
                1 => " One value was adjusted to its valid range.".to_string(),
                n => format!(" {n} values were adjusted to their valid range."),
            };
            m.message = Some((
                true,
                format!("Imported \u{201C}{}\u{201D} as {id}.{note}", prep.preset.name),
            ));
            m.pending = None;
            m.selected = Some(id);
            Some(Library::load(Some(dir)))
        }
        Err(e) => {
            m.message = Some((false, e));
            None
        }
    }
}

/// Exports the preset `id` to `path` and says where it went.
pub fn export_path(m: &mut FilesModel, lib: &Library, id: &str, path: &Path) {
    m.message = Some(match lib.find(id) {
        None => (false, format!("No preset \"{id}\".")),
        Some(p) => match files::export_to(p, path) {
            Ok(written) => (true, format!("Exported to {}.", written.display())),
            Err(e) => (false, e),
        },
    });
}

fn small(ui: &Frame, colour: theme::Rgba, text: &str) {
    theme::with_font(ui, None, METRICS.font_small, || {
        let wrap = ui.push_text_wrap_pos(0.0);
        ui.text_colored(colour, text);
        drop(wrap);
    });
}

/// The section. `dir` is the user preset folder (`None`: no settings folder, nothing can be managed); `current`
/// the preset in force.
pub fn section(
    ui: &Frame,
    fonts: &Fonts,
    m: &mut FilesModel,
    lib: &Library,
    dir: Option<&Path>,
    current: Option<&str>,
    r: &mut MenuResult,
) {
    theme::subheading(ui, fonts, "Preset files");
    let Some(dir) = dir else {
        small(
            ui,
            PALETTE.text_faint,
            "No settings folder (APPDATA is not set): preset files cannot be managed.",
        );
        return;
    };
    // The preset the tools act on.
    let id = m
        .selected
        .clone()
        .filter(|i| lib.find(i).is_some())
        .or_else(|| current.filter(|i| lib.find(i).is_some()).map(str::to_string))
        .or_else(|| lib.list().first().map(|p| p.id.clone()));
    let Some(id) = id else {
        return;
    };
    let preset = lib.find(&id).expect("chosen from the library").clone();
    let user = matches!(preset.origin, Origin::User(_));
    let width = theme::row_label(ui, "Preset");
    ui.set_next_item_width(width);
    if let Some(_combo) = ui.begin_combo("##files_preset", &preset.name) {
        for (i, p) in lib.list().iter().enumerate() {
            let tag = if matches!(p.origin, Origin::User(_)) {
                "  \u{00B7}  user"
            } else {
                ""
            };
            if ui
                .selectable_config(format!("{}{tag}##files{i}", p.name))
                .selected(p.id == id)
                .build()
            {
                m.selected = Some(p.id.clone());
                m.confirm_delete = None;
                m.rename.clear();
            }
            record(ui, r, &format!("files:preset:{i}"));
        }
    }
    record(ui, r, "files:preset");
    let what = match &preset.origin {
        Origin::User(path) => format!("user preset  \u{00B7}  {}", path.display()),
        Origin::Embedded => "shipped preset (read only; duplicate it to change it)".to_string(),
    };
    theme::with_font(ui, None, METRICS.font_small, || {
        ui.set_cursor_pos_x(ui.cursor_pos()[0] + theme::label_width(ui));
        let wrap = ui.push_text_wrap_pos(0.0);
        ui.text_colored(PALETTE.text_muted, &what);
        drop(wrap);
    });
    ui.dummy([0.0, 2.0 * theme::scale_of(ui)]);

    // Duplicate, Export, Export to file..., Delete.
    if theme::button(ui, "Duplicate") {
        match files::duplicate(lib, dir, &id) {
            Ok((new_id, name)) => {
                m.message = Some((true, format!("Saved a copy: \u{201C}{name}\u{201D} ({new_id}.toml).")));
                m.selected = Some(new_id);
                r.library = Some(Library::load(Some(dir)));
            }
            Err(e) => m.message = Some((false, e)),
        }
    }
    record(ui, r, "files:duplicate");
    if ui.is_item_hovered() {
        theme::tooltip(ui, "Save a copy of this preset as a new user preset.");
    }
    same_line_if_fits(ui, "Export");
    let can_export = m.export_dir.is_some();
    let disabled = ui.begin_disabled_with_cond(!can_export);
    if theme::button(ui, "Export") {
        if let Some(out) = &m.export_dir {
            m.message = Some(match files::export(&preset, out) {
                Ok(path) => (true, format!("Exported to {}.", path.display())),
                Err(e) => (false, e),
            });
        }
    }
    drop(disabled);
    record(ui, r, "files:export");
    if ui.is_item_hovered() {
        theme::tooltip(
            ui,
            "Write this preset as one .toml file into the export folder (shown below), to share or keep.",
        );
    }
    if m.dialogs {
        same_line_if_fits(ui, "Export to file...");
        if theme::button(ui, "Export to file...") {
            r.dialog = Some(DialogRequest::Export(id.clone()));
        }
        record(ui, r, "files:export_dialog");
    }
    if user {
        same_line_if_fits(ui, "Delete");
        let in_force = current == Some(id.as_str());
        if theme::button(ui, "Delete") {
            if in_force {
                m.message = Some((
                    false,
                    "This preset is in use: choose another preset first, then delete it.".to_string(),
                ));
            } else {
                m.confirm_delete = Some(id.clone());
            }
        }
        record(ui, r, "files:delete");
    }
    if m.confirm_delete.as_deref() == Some(id.as_str()) {
        theme::callout(
            ui,
            PALETTE.problem,
            &[format!(
                "Delete \u{201C}{}\u{201D}? Its file is removed for good. Shipped presets are never deleted.",
                preset.name
            )],
        );
        if theme::button_tinted(ui, "Delete preset", PALETTE.problem) {
            m.confirm_delete = None;
            match files::delete(lib, &id) {
                Ok(path) => {
                    m.message = Some((true, format!("Deleted {}.", path.display())));
                    m.selected = None;
                    r.library = Some(Library::load(Some(dir)));
                }
                Err(e) => m.message = Some((false, e)),
            }
        }
        record(ui, r, "files:delete_confirm");
        ui.same_line();
        if theme::button(ui, "Cancel##delete") {
            m.confirm_delete = None;
        }
        record(ui, r, "files:delete_cancel");
    }
    if user {
        let rename_w = ui.calc_text_size("Rename")[0] + 20.0 * theme::scale_of(ui);
        let avail = ui.content_region_avail()[0];
        ui.set_next_item_width((avail - rename_w - ui.clone_style().item_spacing()[0]).max(60.0));
        ui.input_text("##files_rename", &mut m.rename).hint("New name").build();
        record(ui, r, "files:rename_name");
        ui.same_line();
        if theme::button(ui, "Rename") {
            match files::rename(lib, dir, &id, &m.rename) {
                Ok(new_id) => {
                    m.message = Some((
                        true,
                        format!("Renamed to \u{201C}{}\u{201D} ({new_id}.toml).", m.rename.trim()),
                    ));
                    m.rename.clear();
                    m.selected = Some(new_id.clone());
                    let new_lib = Library::load(Some(dir));
                    if current == Some(id.as_str()) {
                        // The preset in force was renamed: select it under its new id (same parameters).
                        r.preset = Some(new_id);
                        r.params_changed = true;
                    }
                    r.library = Some(new_lib);
                }
                Err(e) => m.message = Some((false, e)),
            }
        }
        record(ui, r, "files:rename");
    }
    if let Some(out) = &m.export_dir {
        small(ui, PALETTE.text_faint, &format!("Exports go to {}", out.display()));
    }

    // Import.
    theme::subheading(ui, fonts, "Import");
    if let Some(inbox) = m.import_dir.clone() {
        small(
            ui,
            PALETTE.text_muted,
            &format!(
                "Put .toml preset files in {}, then Rescan (at most {} KB each).",
                inbox.display(),
                files::MAX_IMPORT_BYTES / 1024
            ),
        );
        if theme::button(ui, "Rescan") {
            m.import_rows = Some(files::scan(&inbox));
            let n = m.import_rows.as_ref().map_or(0, Vec::len);
            m.message = Some((true, format!("{n} preset file(s) in the import folder.")));
        }
        record(ui, r, "files:rescan");
    }
    if m.dialogs {
        if m.import_dir.is_some() {
            ui.same_line();
        }
        if theme::button(ui, "Import file...") {
            r.dialog = Some(DialogRequest::Import);
        }
        record(ui, r, "files:import_dialog");
    }
    let rows = m.import_rows.clone().unwrap_or_default();
    let mut picked = None;
    for (i, (path, size)) in rows.iter().enumerate() {
        let key = format!("import{i}");
        let _id = ui.push_id(&key);
        let name = path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let label_w = theme::row_label(ui, &name);
        let _ = label_w;
        theme::with_font(ui, None, METRICS.font_small, || {
            ui.align_text_to_frame_padding();
            ui.text_colored(PALETTE.text_faint, format!("{:.1} KB", *size as f64 / 1024.0));
        });
        ui.same_line();
        let import_w = ui.calc_text_size("Import")[0] + 20.0 * theme::scale_of(ui);
        let right = ui.cursor_pos()[0] + ui.content_region_avail()[0] - import_w;
        ui.same_line_with_pos(right.max(ui.cursor_pos()[0]));
        if theme::button(ui, "Import") {
            picked = Some(path.clone());
        }
        record(ui, r, &format!("files:import:{i}"));
    }
    if let Some(path) = picked {
        if let Some(new_lib) = import_path(m, lib, dir, &path) {
            r.library = Some(new_lib);
        }
    }
    if let Some(prep) = m.pending.clone() {
        theme::callout(
            ui,
            PALETTE.accent,
            &[format!(
                "\u{201C}{}\u{201D} has the id of your user preset {}. Replace it, or import it as a copy?",
                prep.preset.name, prep.preset.id
            )],
        );
        if theme::button_tinted(ui, "Replace", PALETTE.accent) {
            if let Some(new_lib) = install(m, lib, dir, &prep, ImportMode::Replace) {
                r.library = Some(new_lib);
            }
        }
        record(ui, r, "files:replace");
        ui.same_line();
        if theme::button(ui, "Import a copy") {
            if let Some(new_lib) = install(m, lib, dir, &prep, ImportMode::Copy) {
                r.library = Some(new_lib);
            }
        }
        record(ui, r, "files:import_copy");
        ui.same_line();
        if theme::button(ui, "Cancel##import") {
            m.pending = None;
            m.message = Some((true, "Not imported.".to_string()));
        }
        record(ui, r, "files:import_cancel");
    }
    if let Some((ok, text)) = &m.message {
        small(ui, if *ok { PALETTE.ok } else { PALETTE.problem }, text);
        record(ui, r, "files:message");
    }
}
