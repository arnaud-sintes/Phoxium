//! User preset files as the menu manages them (owner request 2026-11-20): rename, delete, duplicate, export and
//! import. Pure decisions plus plain file operations on the user preset folder (`presets::user_dir`), the export
//! folder and the import folder (both next to `settings.toml`); no window, no dialog (the optional Save and Open
//! dialogs are `crate::shell`, and only hand a path to the functions here).
//!
//! Rules:
//! - Only user presets are renamed or deleted; a shipped preset is never written or removed (a user file with a
//!   shipped id replaces it in the list, and deleting that file brings the shipped one back).
//! - Duplicate and export work on any preset (a shipped one becomes a user copy, or a file to share).
//! - Import reads one `.toml` file of at most [`MAX_IMPORT_BYTES`] through the same parser as every preset
//!   (`Preset::parse`: unknown keys and ids refused, values outside the hard range clamped with a warning), cleans
//!   the name ([`clean_name`]), and writes it to the user folder in the normal form (`Preset::to_toml`). An id that
//!   a user preset already has is replaced only with [`ImportMode::Replace`] (the menu asks first); an id of a
//!   shipped preset is imported under a free id (`<id>-2`), never over the shipped one.

use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::presets::{preset_id_for, Library, Origin, Preset, MAX_PRESET_NAME};

/// Largest preset file imported (the shipped presets are about 5 KB).
pub const MAX_IMPORT_BYTES: u64 = 256 * 1024;

/// The export folder next to `settings.toml`.
pub fn export_dir(settings_dir: &Path) -> PathBuf {
    settings_dir.join("export")
}

/// The import (drop) folder next to `settings.toml`.
pub fn import_dir(settings_dir: &Path) -> PathBuf {
    settings_dir.join("import")
}

/// `name` made safe to show and store: control characters removed, spaces trimmed, at most
/// [`MAX_PRESET_NAME`] characters.
pub fn clean_name(name: &str) -> String {
    let s: String = name.chars().filter(|c| !c.is_control()).collect();
    s.trim()
        .chars()
        .take(MAX_PRESET_NAME)
        .collect::<String>()
        .trim()
        .to_string()
}

/// Checks a name typed in the menu and gives its id: not empty, at most [`MAX_PRESET_NAME`] characters, no control
/// character, at least one letter or digit.
pub fn check_name(name: &str) -> Result<(String, String), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Type a name.".to_string());
    }
    if name.chars().count() > MAX_PRESET_NAME {
        return Err(format!("The name is longer than {MAX_PRESET_NAME} characters."));
    }
    if name.chars().any(char::is_control) {
        return Err("The name contains a control character.".to_string());
    }
    let id = preset_id_for(name).ok_or("The name needs at least one letter or digit (A to Z, 0 to 9).")?;
    Ok((name.to_string(), id))
}

/// The user preset `id` and its file.
fn user_preset<'a>(lib: &'a Library, id: &str) -> Result<(&'a Preset, &'a Path), String> {
    let p = lib.find(id).ok_or_else(|| format!("No preset \"{id}\"."))?;
    match &p.origin {
        Origin::User(path) if !path.as_os_str().is_empty() => Ok((p, path.as_path())),
        _ => Err("Shipped presets cannot be renamed or deleted; duplicate it first.".to_string()),
    }
}

/// An id free in `lib` and in `dir`: `base`, else `base-2`, `base-3`...
pub fn free_id(lib: &Library, dir: &Path, base: &str) -> String {
    let taken = |id: &str| lib.find(id).is_some() || dir.join(format!("{id}.toml")).exists();
    if !taken(base) {
        return base.to_string();
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|id| !taken(id))
        .expect("an unbounded range")
}

fn save_as(p: &Preset, dir: &Path, id: &str, name: &str) -> Result<PathBuf, String> {
    let copy = Preset {
        id: id.to_string(),
        name: name.to_string(),
        ..p.clone()
    };
    copy.save(dir).map_err(|e| format!("Cannot write {id}.toml: {e}"))
}

/// Renames the user preset `id` to `new_name`: the file is rewritten under the new id (`<new id>.toml`) and the
/// old file removed; a new id that another preset has is refused. Returns the new id.
pub fn rename(lib: &Library, dir: &Path, id: &str, new_name: &str) -> Result<String, String> {
    let (p, old_path) = user_preset(lib, id)?;
    let (name, new_id) = check_name(new_name)?;
    if new_id != id {
        if let Some(other) = lib.find(&new_id) {
            return Err(match other.origin {
                Origin::Embedded => format!("\"{new_id}\" is a shipped preset; choose another name."),
                Origin::User(_) => format!("A user preset \"{new_id}\" exists; choose another name."),
            });
        }
    }
    let written = save_as(p, dir, &new_id, &name)?;
    if written != old_path {
        fs::remove_file(old_path).map_err(|e| format!("Saved {new_id}.toml, but the old file stays: {e}"))?;
    }
    Ok(new_id)
}

/// Deletes the file of the user preset `id` (never a shipped preset). Returns the path removed.
pub fn delete(lib: &Library, id: &str) -> Result<PathBuf, String> {
    let (_, path) = user_preset(lib, id)?;
    fs::remove_file(path).map_err(|e| format!("Cannot delete {}: {e}", path.display()))?;
    Ok(path.to_path_buf())
}

/// Copies the preset `id` (shipped or user) to a new user preset named `<name> copy` (`copy 2`... when taken).
/// Returns the new id and name.
pub fn duplicate(lib: &Library, dir: &Path, id: &str) -> Result<(String, String), String> {
    let p = lib.find(id).ok_or_else(|| format!("No preset \"{id}\"."))?;
    let base: String = p.name.chars().take(MAX_PRESET_NAME - 8).collect();
    let (name, new_id) = (1..1000)
        .map(|n| {
            if n == 1 {
                format!("{} copy", base.trim())
            } else {
                format!("{} copy {n}", base.trim())
            }
        })
        .filter_map(|n| check_name(&n).ok())
        .find(|(_, i)| lib.find(i).is_none() && !dir.join(format!("{i}.toml")).exists())
        .ok_or("No free name for the copy.")?;
    save_as(p, dir, &new_id, &name)?;
    Ok((new_id, name))
}

/// Writes the preset `p` as one `.toml` file at `path` (any preset; the folder is created). The text is the
/// normal preset form, which [`read_import`] reads back.
pub fn export_to(p: &Preset, path: &Path) -> Result<PathBuf, String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("Cannot create {}: {e}", parent.display()))?;
    }
    fs::write(path, p.to_toml()).map_err(|e| format!("Cannot write {}: {e}", path.display()))?;
    Ok(path.to_path_buf())
}

/// Exports `p` as `<dir>\<id>.toml` (an older export of the same preset is replaced).
pub fn export(p: &Preset, dir: &Path) -> Result<PathBuf, String> {
    export_to(p, &dir.join(format!("{}.toml", p.id)))
}

/// What an imported id collides with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Conflict {
    None,
    /// A user preset has this id: replacing it needs a confirmation.
    User,
    /// A shipped preset has this id: the import takes a free id instead.
    Shipped,
}

/// A preset file read and checked, ready to install ([`install`]).
#[derive(Clone, Debug)]
pub struct Prepared {
    pub preset: Preset,
    pub file: PathBuf,
    pub conflict: Conflict,
    /// Values clamped or outside their usual range (the parser's warnings).
    pub warnings: Vec<String>,
}

/// Reads and checks the preset file `path` for an import: at most [`MAX_IMPORT_BYTES`], UTF-8, a valid preset
/// (`Preset::parse`), the name cleaned ([`clean_name`]; empty: the id).
pub fn read_import(path: &Path, lib: &Library) -> Result<Prepared, String> {
    let shown = path
        .file_name()
        .map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
    let too_big = |size: u64| {
        format!(
            "{shown} is {} KB, more than the {} KB a preset can be: not imported.",
            size.div_ceil(1024),
            MAX_IMPORT_BYTES / 1024
        )
    };
    let size = fs::metadata(path)
        .map_err(|e| format!("Cannot read {shown}: {e}"))?
        .len();
    if size > MAX_IMPORT_BYTES {
        return Err(too_big(size));
    }
    // The read itself is bounded too: the size above can be stale (a file still growing) or not the real one (a
    // pipe or a device name reports 0), and nothing past the limit is ever held in memory.
    let mut bytes = Vec::new();
    fs::File::open(path)
        .and_then(|f| {
            use std::io::Read;
            f.take(MAX_IMPORT_BYTES + 1).read_to_end(&mut bytes)
        })
        .map_err(|e| format!("Cannot read {shown}: {e}"))?;
    if bytes.len() as u64 > MAX_IMPORT_BYTES {
        return Err(too_big(bytes.len() as u64));
    }
    let text = String::from_utf8(bytes).map_err(|_| format!("{shown} is not a text file: not imported."))?;
    let mut preset = Preset::parse(&shown, &text, Origin::User(path.to_path_buf())).map_err(|e| e.to_string())?;
    let name = clean_name(&preset.name);
    preset.name = if name.is_empty() { preset.id.clone() } else { name };
    let conflict = match lib.find(&preset.id).map(|p| &p.origin) {
        None => Conflict::None,
        Some(Origin::User(_)) => Conflict::User,
        Some(Origin::Embedded) => Conflict::Shipped,
    };
    Ok(Prepared {
        warnings: preset.warnings.clone(),
        preset,
        file: path.to_path_buf(),
        conflict,
    })
}

/// How an import treats an id that a user preset already has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportMode {
    /// Refuse it (the menu then asks).
    New,
    /// Replace the user preset of that id (confirmed).
    Replace,
    /// Import under a free id, the name marked `(imported)`.
    Copy,
}

/// Writes a [`Prepared`] preset to the user folder `dir`. Returns the id it was stored under. A shipped id always
/// gets a free id; a user id needs [`ImportMode::Replace`] or [`ImportMode::Copy`].
pub fn install(prep: &Prepared, lib: &Library, dir: &Path, mode: ImportMode) -> Result<String, String> {
    let p = &prep.preset;
    let copy_name = || {
        let base: String = p.name.chars().take(MAX_PRESET_NAME - 11).collect();
        format!("{} (imported)", base.trim())
    };
    let (id, name) = match (prep.conflict, mode) {
        (Conflict::None, _) => (p.id.clone(), p.name.clone()),
        (Conflict::User, ImportMode::New) => {
            return Err(format!(
                "A user preset \"{}\" exists: replace it, or import a copy.",
                p.id
            ))
        }
        (Conflict::User, ImportMode::Replace) => {
            // The file of the preset replaced goes, if it is not where the import writes.
            if let Some(Preset {
                origin: Origin::User(old),
                ..
            }) = lib.find(&p.id)
            {
                if *old != dir.join(format!("{}.toml", p.id)) && old.exists() {
                    let _ = fs::remove_file(old);
                }
            }
            (p.id.clone(), p.name.clone())
        }
        (Conflict::Shipped, _) | (Conflict::User, ImportMode::Copy) => (free_id(lib, dir, &p.id), copy_name()),
    };
    save_as(p, dir, &id, &name)?;
    Ok(id)
}

/// The `.toml` files of the import folder `dir` with their size in bytes, sorted by name (none when the folder
/// is missing).
pub fn scan(dir: &Path) -> Vec<(PathBuf, u64)> {
    let Ok(rd) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<(PathBuf, u64)> = rd
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("toml")))
        .map(|p| {
            let n = fs::metadata(&p).map_or(0, |m| m.len());
            (p, n)
        })
        .collect();
    v.sort();
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::Params;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("phoxium-presetfiles-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn user(dir: &Path, name: &str) -> (Library, String) {
        let (_, id) = check_name(name).unwrap();
        let p = Params {
            exposure: 1.5,
            ..Params::default()
        };
        Preset::from_params(&id, name, &p).unwrap().save(dir).unwrap();
        (Library::load(Some(dir)), id)
    }

    fn shipped(lib: &Library) -> String {
        lib.list()
            .iter()
            .find(|p| p.origin == Origin::Embedded)
            .unwrap()
            .id
            .clone()
    }

    #[test]
    fn names_are_checked_and_cleaned() {
        assert_eq!(check_name("  My Tube 2 "), Ok(("My Tube 2".into(), "my-tube-2".into())));
        assert!(check_name("").is_err());
        assert!(check_name("!!!").unwrap_err().contains("letter or digit"));
        assert!(check_name("a\u{7}b").unwrap_err().contains("control"));
        assert!(check_name(&"x".repeat(61)).unwrap_err().contains("longer"));
        assert_eq!(clean_name(" A\u{1}B\tC "), "ABC");
        assert_eq!(clean_name(&"y".repeat(100)).chars().count(), MAX_PRESET_NAME);
    }

    #[test]
    fn rename_moves_the_file_and_refuses_shipped_and_taken_names() {
        let dir = temp("rename");
        let (lib, id) = user(&dir, "Mine");
        let new = rename(&lib, &dir, &id, "Mine Better").unwrap();
        assert_eq!(new, "mine-better");
        assert!(!dir.join("mine.toml").exists());
        let lib = Library::load(Some(&dir));
        let p = lib.find("mine-better").unwrap();
        assert_eq!((p.name.as_str(), p.params().exposure), ("Mine Better", 1.5));
        // The same id, another spelling: the name changes in place.
        assert_eq!(rename(&lib, &dir, "mine-better", "MINE better").unwrap(), "mine-better");
        assert_eq!(
            Library::load(Some(&dir)).find("mine-better").unwrap().name,
            "MINE better"
        );
        // A shipped preset is never renamed; a shipped id is never taken.
        let s = shipped(&lib);
        assert!(rename(&lib, &dir, &s, "x").unwrap_err().contains("Shipped"));
        let shipped_name = lib.find(&s).unwrap().id.clone();
        assert!(rename(&lib, &dir, "mine-better", &shipped_name)
            .unwrap_err()
            .contains("shipped"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_removes_only_user_files() {
        let dir = temp("delete");
        let (lib, id) = user(&dir, "Gone Soon");
        let s = shipped(&lib);
        assert!(delete(&lib, &s).unwrap_err().contains("Shipped"));
        let path = delete(&lib, &id).unwrap();
        assert!(!path.exists());
        let lib = Library::load(Some(&dir));
        assert!(lib.find(&id).is_none());
        assert!(lib.find(&s).is_some(), "the shipped presets are untouched");
        assert!(delete(&lib, "nope").is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn duplicate_copies_any_preset_under_a_free_name() {
        let dir = temp("dup");
        let lib = Library::load(Some(&dir));
        let s = shipped(&lib);
        let (id, name) = duplicate(&lib, &dir, &s).unwrap();
        assert!(name.ends_with(" copy"), "{name}");
        let lib = Library::load(Some(&dir));
        assert_eq!(lib.find(&id).unwrap().params(), lib.find(&s).unwrap().params());
        let (id2, name2) = duplicate(&lib, &dir, &s).unwrap();
        assert!(name2.ends_with(" copy 2") && id2 != id, "{name2} {id2}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn export_then_import_round_trips_and_conflicts_are_handled() {
        let dir = temp("export");
        let users = dir.join("presets");
        let (lib, id) = user(&users, "Shared Tube");
        let out = export(lib.find(&id).unwrap(), &export_dir(&dir)).unwrap();
        assert_eq!(out, export_dir(&dir).join("shared-tube.toml"));
        // Read back: a user preset of that id exists, so a plain import is refused.
        let prep = read_import(&out, &lib).unwrap();
        assert_eq!(
            (prep.conflict, prep.preset.id.as_str()),
            (Conflict::User, "shared-tube")
        );
        assert!(install(&prep, &lib, &users, ImportMode::New)
            .unwrap_err()
            .contains("exists"));
        assert_eq!(
            install(&prep, &lib, &users, ImportMode::Replace).unwrap(),
            "shared-tube"
        );
        let copy = install(&prep, &lib, &users, ImportMode::Copy).unwrap();
        assert_eq!(copy, "shared-tube-2");
        let lib2 = Library::load(Some(&users));
        assert_eq!(lib2.find(&copy).unwrap().name, "Shared Tube (imported)");
        assert_eq!(lib2.find(&copy).unwrap().params(), lib2.find(&id).unwrap().params());
        // Into an empty library: no conflict.
        let empty = Library::embedded();
        let fresh = temp("export-fresh");
        let prep = read_import(&out, &empty).unwrap();
        assert_eq!(prep.conflict, Conflict::None);
        assert_eq!(install(&prep, &empty, &fresh, ImportMode::New).unwrap(), "shared-tube");
        // A shipped id is imported under a free id, never over the shipped preset.
        let s = shipped(&empty);
        let shipped_file = export(empty.find(&s).unwrap(), &dir.join("x")).unwrap();
        let prep = read_import(&shipped_file, &empty).unwrap();
        assert_eq!(prep.conflict, Conflict::Shipped);
        assert_eq!(
            install(&prep, &empty, &fresh, ImportMode::New).unwrap(),
            format!("{s}-2")
        );
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&fresh);
    }

    #[test]
    fn imports_are_validated_size_limited_and_their_names_cleaned() {
        let dir = temp("import");
        let lib = Library::embedded();
        let write = |name: &str, text: &str| {
            let p = dir.join(name);
            fs::write(&p, text).unwrap();
            p
        };
        let big = write("big.toml", &"#".repeat(MAX_IMPORT_BYTES as usize + 1));
        assert!(read_import(&big, &lib).unwrap_err().contains("more than the 256 KB"));
        let bad = write(
            "bad.toml",
            "schema = 1\nid = \"x\"\nname = \"x\"\nlabel = \"x\"\nwhat = 1\n[params]\n[provenance]\n",
        );
        assert!(read_import(&bad, &lib).unwrap_err().contains("unknown key 'what'"));
        let unknown = write(
            "unknown.toml",
            "schema = 1\nid = \"x\"\nname = \"x\"\nlabel = \"x\"\n[params]\nnot_a_param = 1.0\n[provenance]\nnot_a_param \
             = \"E: user\"\n",
        );
        assert!(read_import(&unknown, &lib).unwrap_err().contains("unknown parameter"));
        let bin = dir.join("bin.toml");
        fs::write(&bin, [0xFFu8, 0xFE, 0x00]).unwrap();
        assert!(read_import(&bin, &lib).unwrap_err().contains("not a text file"));
        // A value beyond the hard range is clamped with a warning; a control character leaves the name.
        let odd = write(
            "odd.toml",
            "schema = 1\nid = \"odd\"\nname = \"Odd\\u0007 Tube\"\nlabel = \"x\"\n[params]\ntube_visible_height_mm = \
             -5.0\n[provenance]\ntube_visible_height_mm = \"E: user\"\n",
        );
        let prep = read_import(&odd, &lib).unwrap();
        assert_eq!(prep.preset.name, "Odd Tube");
        assert!(!prep.warnings.is_empty());
        assert!(prep.preset.params().tube_visible_height_mm > 0.0);
        // The folder scan lists the .toml files only.
        write("notes.txt", "x");
        let names: Vec<String> = scan(&dir)
            .iter()
            .map(|(p, _)| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["bad.toml", "big.toml", "bin.toml", "odd.toml", "unknown.toml"]);
        assert!(scan(&dir.join("missing")).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    /// Hostile import files: exactly at the limit (read, then refused as a preset, not as a size), one byte over,
    /// deep nesting (the parser's recursion stays bounded: an error, never a stack overflow), a huge name within
    /// the limit (cut), a folder named like a preset, a missing file. Every one gives a message, none panics.
    #[test]
    fn hostile_import_files_are_refused_with_a_message() {
        let dir = temp("hostile");
        let lib = Library::embedded();
        let write = |name: &str, text: &str| {
            let p = dir.join(name);
            fs::write(&p, text).unwrap();
            p
        };
        let at = write("at.toml", &"#".repeat(MAX_IMPORT_BYTES as usize));
        let e = read_import(&at, &lib).unwrap_err();
        assert!(!e.contains("KB"), "at the limit the size is fine: {e}");
        let over = write("over.toml", &"#".repeat(MAX_IMPORT_BYTES as usize + 1));
        assert!(read_import(&over, &lib).unwrap_err().contains("not imported"));
        for (name, text) in [
            (
                "arrays.toml",
                format!("x = {}{}", "[".repeat(100_000), "]".repeat(100_000)),
            ),
            (
                "tables.toml",
                format!("x = {}{}", "{a = ".repeat(40_000), "}".repeat(40_000)),
            ),
        ] {
            let p = write(name, &text);
            assert!(read_import(&p, &lib).is_err(), "{name}");
        }
        let long_name = format!(
            "schema = 1\nid = \"long\"\nname = \"{}\"\nlabel = \"x\"\n[params]\n[provenance]\n",
            "N".repeat(200_000)
        );
        let p = write("long.toml", &long_name);
        if let Ok(prep) = read_import(&p, &lib) {
            assert!(prep.preset.name.chars().count() <= MAX_PRESET_NAME);
        }
        let folder = dir.join("folder.toml");
        fs::create_dir_all(&folder).unwrap();
        assert!(read_import(&folder, &lib).unwrap_err().contains("Cannot read"));
        assert!(read_import(&dir.join("missing.toml"), &lib)
            .unwrap_err()
            .contains("Cannot read"));
        let _ = fs::remove_dir_all(&dir);
    }
}
