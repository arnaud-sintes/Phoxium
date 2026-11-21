//! What the menu's About section shows (owner request 2026-11-20), all known at build time, no network: the
//! version, the commit the build came from (`build.rs` embeds `git rev-parse --short HEAD` when the repository is
//! there, else `unknown`; no build date, so a build stays reproducible), the licence line, the project address,
//! the third-party table (`assets/third-party.tsv`, written by `scripts\gen-third-party.ps1 -TableFile`, checked
//! against `Cargo.lock` by a test) and the notice on unsigned binaries. The full licence texts stay in the
//! release zip's `THIRD-PARTY.md`.

/// The application version (`Cargo.toml`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The commit the build came from, or `unknown` (built outside a git checkout).
pub const COMMIT: &str = env!("PHOXIUM_COMMIT");

/// The project address, shown as plain text (the app opens nothing).
pub const PROJECT_URL: &str = "https://github.com/arnaud-sintes/Phoxium";

/// The licence line (as in `LICENSE`).
pub const LICENCE: &str = "MIT licence. Copyright (c) 2026 Arnaud Sint\u{00E8}s.";

/// The note on unsigned executables (README "Unsigned binaries").
pub const UNSIGNED_NOTE: &str = "The executables are not code-signed, so Windows SmartScreen may warn the first time. \
                                 Check the download against the SHA-256 published with the release.";

/// The compact third-party table (generated, committed).
const TABLE: &str = include_str!("../assets/third-party.tsv");

/// The C and C++ sources and the font the crates do not list (their notices are in `THIRD-PARTY.md`).
pub const BUNDLED: &[(&str, &str, &str)] = &[
    ("Dear ImGui (C++, inside dear-imgui-sys)", "1.93.1 WIP", "MIT"),
    ("cimgui (C, inside dear-imgui-sys)", "", "MIT"),
    ("stb_truetype, stb_rectpack, stb_textedit", "", "MIT"),
    ("ProggyClean font (inside Dear ImGui)", "", "MIT"),
    ("Inter font", "4.1", "OFL-1.1"),
];

/// One third-party package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Package {
    pub name: String,
    pub version: String,
    /// The SPDX licence expression it declares.
    pub licence: String,
}

/// The packages of a table text: tab-separated name, version, licence; `#` lines and blank lines skipped.
pub fn parse_table(text: &str) -> Vec<Package> {
    text.lines()
        .map(str::trim_end)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let mut f = l.split('\t');
            Some(Package {
                name: f.next()?.to_string(),
                version: f.next()?.to_string(),
                licence: f.next()?.to_string(),
            })
        })
        .collect()
}

/// The crates built into or with the executables.
pub fn crates() -> Vec<Package> {
    parse_table(TABLE)
}

/// The version line: `0.1.0 (commit 1a2b3c4)`.
pub fn version_line() -> String {
    format!("{VERSION} (commit {COMMIT})")
}

/// The first-run hint from the chords in force (`None`: that action has no key).
pub fn first_run_text(menu: Option<&str>, fullscreen: Option<&str>, quit: Option<&str>) -> String {
    let part = |key: Option<&str>, what: &str| match key {
        Some(k) => format!("{k} {what}"),
        None => format!("{what}: no key (see the menu)"),
    };
    let menu = match menu {
        Some(k) => format!("Press {k} for the menu"),
        None => "The menu has no key (another program took it)".to_string(),
    };
    format!(
        "{menu}, {}, {} to quit.",
        part(fullscreen, "for fullscreen"),
        quit.unwrap_or("the close button")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_table_lists_cargo_lock_packages_with_permissive_licences() {
        let rows = crates();
        assert!(rows.len() >= 30, "{} rows", rows.len());
        let lock = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.lock"))
            .expect("Cargo.lock in the workspace");
        for p in &rows {
            let entry = format!("name = \"{}\"\nversion = \"{}\"", p.name, p.version);
            assert!(
                lock.replace("\r\n", "\n").contains(&entry),
                "{} {} is not in Cargo.lock: run scripts\\gen-third-party.ps1 -TableFile crates\\phoxium\\assets\\third-party.tsv",
                p.name,
                p.version
            );
            assert!(!p.licence.is_empty() && !p.licence.contains("GPL"), "{p:?}");
        }
        for name in ["ash", "dear-imgui-rs", "toml", "windows"] {
            assert!(rows.iter().any(|p| p.name == name), "{name} missing from the table");
        }
    }

    #[test]
    fn the_table_parser_skips_comments_and_short_lines() {
        let t = "# head\n\nash\t0.38\tMIT OR Apache-2.0\r\nbroken\t1\n";
        assert_eq!(
            parse_table(t),
            [Package {
                name: "ash".into(),
                version: "0.38".into(),
                licence: "MIT OR Apache-2.0".into()
            }]
        );
    }

    #[test]
    fn version_and_first_run_lines_read_well() {
        assert!(version_line().starts_with(VERSION));
        assert!(
            !COMMIT.is_empty() && COMMIT.chars().all(|c| c.is_ascii_alphanumeric()),
            "{COMMIT}"
        );
        assert_eq!(
            first_run_text(Some("F10"), Some("F11"), Some("Ctrl+Alt+End")),
            "Press F10 for the menu, F11 for fullscreen, Ctrl+Alt+End to quit."
        );
        let t = first_run_text(Some("Ctrl+Alt+F10"), None, Some("Ctrl+Alt+End"));
        assert!(
            t.starts_with("Press Ctrl+Alt+F10 for the menu") && t.contains("no key"),
            "{t}"
        );
    }
}
