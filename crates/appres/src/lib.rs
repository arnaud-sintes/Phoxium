//! Application icon and Windows resources, without rc.exe or any dependency.
//!
//! - [`ico`]: writes and reads multi-resolution `.ico` files (PNG frames for 64 px and up, 32-bit BMP frames with
//!   an AND mask below).
//! - [`res`]: writes a Win32 `.res` file (RT_ICON frames, an RT_GROUP_ICON and a VERSIONINFO) that the MSVC
//!   linker accepts as an input (`cargo:rustc-link-arg-bins=<file>.res`; link.exe converts it with CVTRES).
//! - [`pe`]: reads the resource directory of a built PE executable, for the tests that check what was embedded.
//!
//! The icon artwork itself is drawn by the `gen_icon` example (`assets/icon/README.md`).

pub mod ico;
pub mod pe;
pub mod res;

/// Resource type identifiers (`winuser.h`).
pub mod rt {
    pub const ICON: u16 = 3;
    pub const GROUP_ICON: u16 = 14;
    pub const VERSION: u16 = 16;
}

/// Product name in every VERSIONINFO.
pub const PRODUCT_NAME: &str = "Phoxium";
/// Copyright line in every VERSIONINFO (personal project, no company).
pub const COPYRIGHT: &str = "Copyright (C) 2026, Phoxium personal project";

/// Build-script helper: embeds the `.ico` at `ico` as the executable icon (RT_GROUP_ICON 1, the icon Explorer,
/// the taskbar and Alt+Tab show; also what `LoadImageW(hinstance, 1, IMAGE_ICON, ..)` loads) and a VERSIONINFO
/// with the package version, into every binary of the calling package. Writes `<OUT_DIR>/<exe>.res` and passes
/// it to the MSVC linker; does nothing (except the rerun line) for a non-MSVC target.
pub fn embed_for_build_script(ico: &std::path::Path, exe: &str, description: &str) {
    println!("cargo:rerun-if-changed={}", ico.display());
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        return;
    }
    let bytes = std::fs::read(ico).unwrap_or_else(|e| panic!("cannot read {}: {e}", ico.display()));
    let frames = ico::parse(&bytes).unwrap_or_else(|e| panic!("{}: {e}", ico.display()));
    let version = std::env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION");
    let info = res::VersionInfo {
        version: res::VersionInfo::parse_version(&version),
        strings: [
            ("FileDescription", description),
            ("FileVersion", &version),
            ("InternalName", exe),
            ("LegalCopyright", COPYRIGHT),
            ("OriginalFilename", &format!("{exe}.exe")),
            ("ProductName", PRODUCT_NAME),
            ("ProductVersion", &version),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect(),
    };
    let mut file = res::ResFile::new();
    file.icon_group(1, &frames).version_info(&info);
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR")).join(format!("{exe}.res"));
    std::fs::write(&out, file.into_bytes()).unwrap_or_else(|e| panic!("cannot write {}: {e}", out.display()));
    // link.exe takes a .res input directly (it runs CVTRES); bins only, so test harnesses link as before.
    println!("cargo:rustc-link-arg-bins={}", out.display());
}

/// Little-endian readers over a byte slice, `None` when out of range.
pub(crate) fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

pub(crate) fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// UTF-16LE bytes of `s`, without terminator.
pub fn utf16_bytes(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// Whether `haystack` contains `needle` encoded as UTF-16LE (used to look for VERSIONINFO strings).
pub fn contains_utf16(haystack: &[u8], needle: &str) -> bool {
    let n = utf16_bytes(needle);
    !n.is_empty() && haystack.windows(n.len()).any(|w| w == n.as_slice())
}
