//! One-time copy of the settings folder of the earlier versions (up to 0.3.0) into the Phoxium one.
//!
//! The settings folder of the earlier versions was `%APPDATA%\ultimateCRT` (settings, presets, export and import
//! folders, game profiles, burn-in state, crash reports); it became `%APPDATA%\Phoxium` (QUESTIONS REN-1,
//! [`crate::settings::default_dir`]). At start, [`migrate_dir`] copies the old folder when the new one does not
//! exist yet. The rules:
//!
//! - nothing happens when the new folder (or a file of that name) exists, or when there is no old folder;
//! - the old folder is only read, never changed or deleted (an older build keeps working with it);
//! - nothing in the new folder is ever overwritten: the copy is made into a temporary folder next to it, which
//!   is then renamed to the new name, and that rename fails when the new folder appeared in the meantime;
//! - symbolic links and other special entries are skipped (never followed);
//! - a failure is reported, never a panic: the temporary folder is removed, the new folder is not created, the
//!   app starts with the defaults and the next start tries again.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

/// The name of the settings folder under `%APPDATA%` before the rename.
pub const LEGACY_DIR_NAME: &str = "ultimateCRT";

/// `%APPDATA%\ultimateCRT`, or `None` when `APPDATA` is not set.
pub fn legacy_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join(LEGACY_DIR_NAME))
}

/// What [`migrate_dir`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The new folder exists, or there is no old folder: nothing was done.
    NotNeeded,
    /// The old folder was copied: the number of files copied and of entries skipped (links, special files).
    Copied { files: usize, skipped: usize },
    /// The copy failed (the reason); the new folder was not created and the old one is unchanged.
    Failed(String),
}

/// Copies the folder `old` to `new` when `new` does not exist and `old` is a folder (the rules are in the module
/// documentation). Never panics on an I/O problem.
pub fn migrate_dir(old: &Path, new: &Path) -> Outcome {
    // symlink_metadata: a link named like the new folder counts as existing, and is not followed.
    if fs::symlink_metadata(new).is_ok() {
        return Outcome::NotNeeded;
    }
    match fs::symlink_metadata(old) {
        Ok(m) if m.is_dir() => {}
        _ => return Outcome::NotNeeded,
    }
    let Some(name) = new.file_name() else {
        return Outcome::Failed(format!("{} has no folder name", new.display()));
    };
    let mut tmp_name = name.to_os_string();
    tmp_name.push(format!(".migrating-{}", std::process::id()));
    let tmp = new.with_file_name(tmp_name);
    // A folder of that name can only be a leftover of this process id; start from a clean one.
    let _ = fs::remove_dir_all(&tmp);
    let mut counts = (0usize, 0usize);
    let result = fs::create_dir_all(&tmp)
        .and_then(|()| copy_tree(old, &tmp, &mut counts))
        .and_then(|()| {
            // Renaming a folder onto an existing name fails on Windows: a new folder that appeared meanwhile is
            // never overwritten.
            if fs::symlink_metadata(new).is_ok() {
                Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("{} appeared during the copy", new.display()),
                ))
            } else {
                fs::rename(&tmp, new)
            }
        });
    match result {
        Ok(()) => Outcome::Copied {
            files: counts.0,
            skipped: counts.1,
        },
        Err(e) => {
            let _ = fs::remove_dir_all(&tmp);
            Outcome::Failed(format!("cannot copy {} to {}: {e}", old.display(), new.display()))
        }
    }
}

/// Copies the content of the folder `from` into the existing folder `to`, recursively. Regular files and folders
/// only; `counts` is (files copied, entries skipped).
fn copy_tree(from: &Path, to: &Path, counts: &mut (usize, usize)) -> io::Result<()> {
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let target = to.join(entry.file_name());
        if ty.is_symlink() {
            counts.1 += 1;
        } else if ty.is_dir() {
            fs::create_dir(&target)?;
            copy_tree(&entry.path(), &target, counts)?;
        } else if ty.is_file() {
            fs::copy(entry.path(), &target)?;
            counts.0 += 1;
        } else {
            counts.1 += 1;
        }
    }
    Ok(())
}

/// The start-up step: [`migrate_dir`] from [`legacy_dir`] to [`crate::settings::default_dir`]. Returns the one
/// line to print, or `None` when nothing was done (or `APPDATA` is not set).
pub fn migrate_default_dir() -> Option<String> {
    let (old, new) = (legacy_dir()?, crate::settings::default_dir()?);
    message(&migrate_dir(&old, &new), &old, &new)
}

/// The line printed for an outcome, `None` for [`Outcome::NotNeeded`].
fn message(outcome: &Outcome, old: &Path, new: &Path) -> Option<String> {
    match outcome {
        Outcome::NotNeeded => None,
        Outcome::Copied { files, skipped } => Some(format!(
            "settings: copied {files} file(s) from {} to {} (settings of the earlier versions; \
             the old folder is kept{})",
            old.display(),
            new.display(),
            if *skipped > 0 {
                format!(", {skipped} link(s) or special file(s) skipped")
            } else {
                String::new()
            }
        )),
        Outcome::Failed(why) => Some(format!(
            "warning: settings: the settings of the earlier versions were not copied ({why}); starting with the defaults, \
             the old folder is unchanged and the next start tries again"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("phoxium-migrate-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// An old folder shaped like a real one: settings, a preset, a burn-in state, an empty export folder.
    fn make_old(root: &Path) -> PathBuf {
        let old = root.join("ultimateCRT");
        fs::create_dir_all(old.join("presets")).unwrap();
        fs::create_dir_all(old.join("tubes")).unwrap();
        fs::create_dir_all(old.join("export")).unwrap();
        fs::write(old.join("settings.toml"), "version = 1\n").unwrap();
        fs::write(old.join("presets").join("mine.toml"), "id = \"mine\"\n").unwrap();
        fs::write(old.join("tubes").join("default.state"), [1u8, 2, 3, 4]).unwrap();
        old
    }

    fn snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in fs::read_dir(&d).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    out.push((p.strip_prefix(dir).unwrap().to_path_buf(), Vec::new()));
                    stack.push(p);
                } else {
                    out.push((p.strip_prefix(dir).unwrap().to_path_buf(), fs::read(&p).unwrap()));
                }
            }
        }
        out.sort();
        out
    }

    #[test]
    fn copies_the_old_folder_once_and_keeps_it() {
        let root = temp_root("copy");
        let old = make_old(&root);
        let before = snapshot(&old);
        let new = root.join("Phoxium");
        assert_eq!(migrate_dir(&old, &new), Outcome::Copied { files: 3, skipped: 0 });
        assert_eq!(
            snapshot(&new),
            before,
            "the new folder is an exact copy, empty folders included"
        );
        assert_eq!(snapshot(&old), before, "the old folder is unchanged");
        // No temporary folder left beside it.
        let names: Vec<_> = fs::read_dir(&root).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names.len(), 2, "{names:?}");
        // Second start: the new folder exists, nothing happens even when the old one changed.
        fs::write(old.join("settings.toml"), "version = 1\n# changed by an older build\n").unwrap();
        assert_eq!(migrate_dir(&old, &new), Outcome::NotNeeded);
        assert_eq!(fs::read_to_string(new.join("settings.toml")).unwrap(), "version = 1\n");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn never_touches_an_existing_new_folder() {
        let root = temp_root("exists");
        let old = make_old(&root);
        let new = root.join("Phoxium");
        fs::create_dir_all(&new).unwrap();
        fs::write(new.join("settings.toml"), "mine").unwrap();
        assert_eq!(migrate_dir(&old, &new), Outcome::NotNeeded);
        assert_eq!(fs::read_to_string(new.join("settings.toml")).unwrap(), "mine");
        assert!(!new.join("presets").exists());
        // A file with the new folder's name counts as existing too.
        let file_new = root.join("PhoxiumFile");
        fs::write(&file_new, "x").unwrap();
        assert_eq!(migrate_dir(&old, &file_new), Outcome::NotNeeded);
        assert_eq!(fs::read_to_string(&file_new).unwrap(), "x");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn nothing_without_an_old_folder() {
        let root = temp_root("none");
        let new = root.join("Phoxium");
        assert_eq!(migrate_dir(&root.join("ultimateCRT"), &new), Outcome::NotNeeded);
        assert!(!new.exists());
        // An old "folder" that is a file is not copied either.
        fs::write(root.join("ultimateCRT"), "x").unwrap();
        assert_eq!(migrate_dir(&root.join("ultimateCRT"), &new), Outcome::NotNeeded);
        assert!(!new.exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_failure_is_reported_and_leaves_nothing_behind() {
        let root = temp_root("fail");
        let old = make_old(&root);
        let before = snapshot(&old);
        // The parent of the new folder is a file: the temporary folder cannot be created.
        fs::write(root.join("blocker"), "x").unwrap();
        let new = root.join("blocker").join("Phoxium");
        match migrate_dir(&old, &new) {
            Outcome::Failed(why) => assert!(why.contains("cannot copy"), "{why}"),
            other => panic!("expected a failure, got {other:?}"),
        }
        assert_eq!(snapshot(&old), before);
        let line = message(&migrate_dir(&old, &new), &old, &new).unwrap();
        assert!(line.starts_with("warning: settings:"), "{line}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_stale_temporary_folder_is_replaced() {
        let root = temp_root("stale");
        let old = make_old(&root);
        let new = root.join("Phoxium");
        let stale = root.join(format!("Phoxium.migrating-{}", std::process::id()));
        fs::create_dir_all(&stale).unwrap();
        fs::write(stale.join("junk.txt"), "junk").unwrap();
        assert_eq!(migrate_dir(&old, &new), Outcome::Copied { files: 3, skipped: 0 });
        assert!(!new.join("junk.txt").exists());
        assert!(!stale.exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn messages() {
        let (old, new) = (Path::new("C:\\a\\ultimateCRT"), Path::new("C:\\a\\Phoxium"));
        assert_eq!(message(&Outcome::NotNeeded, old, new), None);
        let line = message(&Outcome::Copied { files: 4, skipped: 0 }, old, new).unwrap();
        assert!(
            line.starts_with("settings: copied 4 file(s) from C:\\a\\ultimateCRT to C:\\a\\Phoxium"),
            "{line}"
        );
        assert!(!line.contains("skipped"));
        let line = message(&Outcome::Copied { files: 1, skipped: 2 }, old, new).unwrap();
        assert!(line.contains("2 link(s) or special file(s) skipped"), "{line}");
    }

    #[test]
    fn the_folder_names() {
        assert_eq!(LEGACY_DIR_NAME, "ultimateCRT");
        if let (Some(old), Some(new)) = (legacy_dir(), crate::settings::default_dir()) {
            assert_eq!(old.parent(), new.parent());
            assert_eq!(new.file_name().unwrap(), "Phoxium");
        }
    }
}
