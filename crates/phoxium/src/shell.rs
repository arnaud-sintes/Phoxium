//! Thin Win32 glue for the menu (owner request 2026-11-20): copy a text to the clipboard (the About section's
//! settings folder) and the optional native Save and Open dialogs of the preset files (`GetSaveFileNameW`,
//! `GetOpenFileNameW`). No network, no shell execution. The decisions are elsewhere (`preset_files`); the pure
//! helpers here ([`utf16_z`], [`filter_spec`], [`path_from_buffer`]) are unit tested.
//!
//! The dialogs are modal and owned by the output window. They are opened by the app between two frames, while the
//! menu holds the foreground (the menu mode is activating), so the dialog may take the foreground from the output;
//! the UI ignores the output's deactivation meanwhile (`ui::Ui::begin_dialog`). Not verified live: the focus
//! hand-back after a dialog, and the dialogs themselves on the owner's desktop.

use std::path::{Path, PathBuf};

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{HANDLE, HWND};

/// `text` as UTF-16 with a terminating zero.
pub fn utf16_z(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// A dialog filter (`"Preset files (*.toml)\0*.toml\0All files\0*.*\0\0"`) from (label, pattern) pairs.
pub fn filter_spec(pairs: &[(&str, &str)]) -> Vec<u16> {
    let mut v = Vec::new();
    for (label, pattern) in pairs {
        v.extend(label.encode_utf16());
        v.push(0);
        v.extend(pattern.encode_utf16());
        v.push(0);
    }
    v.push(0);
    v
}

/// The path a dialog wrote into its zero-terminated buffer.
pub fn path_from_buffer(buf: &[u16]) -> Option<PathBuf> {
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    (n > 0).then(|| PathBuf::from(String::from_utf16_lossy(&buf[..n])))
}

/// Puts `text` on the clipboard (`CF_UNICODETEXT`), owned by `owner` (the output window; 0 for none).
pub fn copy_text(owner: isize, text: &str) -> Result<(), String> {
    use windows::Win32::Foundation::GlobalFree;
    use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    /// `CF_UNICODETEXT`.
    const CF_UNICODETEXT: u32 = 13;
    let data = utf16_z(text);
    let bytes = data.len() * 2;
    // SAFETY: the standard clipboard sequence; the memory block is filled while locked, and once
    // `SetClipboardData` succeeds the system owns it (freed here only when it fails).
    unsafe {
        let owner = (owner != 0).then_some(HWND(owner as *mut std::ffi::c_void));
        OpenClipboard(owner).map_err(|e| format!("the clipboard is busy ({e})"))?;
        let result = (|| -> Result<(), String> {
            EmptyClipboard().map_err(|e| e.to_string())?;
            let mem = GlobalAlloc(GMEM_MOVEABLE, bytes).map_err(|e| e.to_string())?;
            let ptr = GlobalLock(mem) as *mut u16;
            if ptr.is_null() {
                let _ = GlobalFree(Some(mem));
                return Err("cannot lock the clipboard memory".to_string());
            }
            std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
            let _ = GlobalUnlock(mem);
            if let Err(e) = SetClipboardData(CF_UNICODETEXT, Some(HANDLE(mem.0))) {
                let _ = GlobalFree(Some(mem));
                return Err(e.to_string());
            }
            Ok(())
        })();
        let _ = CloseClipboard();
        result
    }
}

/// Puts a device-independent bitmap (`CF_DIB`: header and pixels, see `crate::snapshot::dib`) on the clipboard, owned
/// by `owner` (the output window; 0 for none).
pub fn copy_dib(owner: isize, dib: &[u8]) -> Result<(), String> {
    use windows::Win32::Foundation::GlobalFree;
    use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    /// `CF_DIB`.
    const CF_DIB: u32 = 8;
    // SAFETY: as `copy_text`: the standard clipboard sequence, the block is filled while locked and owned by the
    // system once `SetClipboardData` succeeds (freed here only when it fails).
    unsafe {
        let owner = (owner != 0).then_some(HWND(owner as *mut std::ffi::c_void));
        OpenClipboard(owner).map_err(|e| format!("the clipboard is busy ({e})"))?;
        let result = (|| -> Result<(), String> {
            EmptyClipboard().map_err(|e| e.to_string())?;
            let mem = GlobalAlloc(GMEM_MOVEABLE, dib.len()).map_err(|e| e.to_string())?;
            let ptr = GlobalLock(mem) as *mut u8;
            if ptr.is_null() {
                let _ = GlobalFree(Some(mem));
                return Err("cannot lock the clipboard memory".to_string());
            }
            std::ptr::copy_nonoverlapping(dib.as_ptr(), ptr, dib.len());
            let _ = GlobalUnlock(mem);
            if let Err(e) = SetClipboardData(CF_DIB, Some(HANDLE(mem.0))) {
                let _ = GlobalFree(Some(mem));
                return Err(e.to_string());
            }
            Ok(())
        })();
        let _ = CloseClipboard();
        result
    }
}

/// Room for the path a dialog returns (characters).
const PATH_CAPACITY: usize = 1024;

fn preset_filter() -> Vec<u16> {
    filter_spec(&[("Preset files (*.toml)", "*.toml"), ("All files", "*.*")])
}

/// The native Save dialog for a preset file: starts in `folder` with `file_name`, asks before replacing a file.
/// `None` when cancelled.
pub fn save_dialog(owner: isize, folder: &Path, file_name: &str) -> Option<PathBuf> {
    use windows::Win32::UI::Controls::Dialogs::{
        GetSaveFileNameW, OFN_EXPLORER, OFN_NOCHANGEDIR, OFN_OVERWRITEPROMPT, OFN_PATHMUSTEXIST, OPENFILENAMEW,
    };
    let mut buf = vec![0u16; PATH_CAPACITY];
    let name = utf16_z(file_name);
    let n = (name.len() - 1).min(PATH_CAPACITY - 1);
    buf[..n].copy_from_slice(&name[..n]);
    let filter = preset_filter();
    let dir = utf16_z(&folder.display().to_string());
    let title = utf16_z("Export preset");
    let ext = utf16_z("toml");
    let mut ofn = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: HWND(owner as *mut std::ffi::c_void),
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: PWSTR(buf.as_mut_ptr()),
        nMaxFile: PATH_CAPACITY as u32,
        lpstrInitialDir: PCWSTR(dir.as_ptr()),
        lpstrTitle: PCWSTR(title.as_ptr()),
        lpstrDefExt: PCWSTR(ext.as_ptr()),
        Flags: OFN_EXPLORER | OFN_OVERWRITEPROMPT | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR,
        ..Default::default()
    };
    // SAFETY: every pointer in `ofn` points into a buffer alive until the call returns.
    let ok = unsafe { GetSaveFileNameW(&mut ofn) }.as_bool();
    ok.then(|| path_from_buffer(&buf)).flatten()
}

/// The native Open dialog for one preset file, starting in `folder`. `None` when cancelled.
pub fn open_dialog(owner: isize, folder: &Path) -> Option<PathBuf> {
    open_file(owner, folder, "Import preset", preset_filter())
}

/// The native Open dialog for one still image (a PNG) to use as the source, starting in `folder`. `None` when
/// cancelled.
pub fn open_image_dialog(owner: isize, folder: &Path) -> Option<PathBuf> {
    open_file(
        owner,
        folder,
        "Open an image as the source",
        filter_spec(&[("PNG images (*.png)", "*.png"), ("All files", "*.*")]),
    )
}

/// The native Open dialog for one existing file: `title`, the `filter` ([`filter_spec`]), starting in `folder`.
fn open_file(owner: isize, folder: &Path, title: &str, filter: Vec<u16>) -> Option<PathBuf> {
    use windows::Win32::UI::Controls::Dialogs::{
        GetOpenFileNameW, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST, OPENFILENAMEW,
    };
    let mut buf = vec![0u16; PATH_CAPACITY];
    let dir = utf16_z(&folder.display().to_string());
    let title = utf16_z(title);
    let mut ofn = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: HWND(owner as *mut std::ffi::c_void),
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: PWSTR(buf.as_mut_ptr()),
        nMaxFile: PATH_CAPACITY as u32,
        lpstrInitialDir: PCWSTR(dir.as_ptr()),
        lpstrTitle: PCWSTR(title.as_ptr()),
        Flags: OFN_EXPLORER | OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR,
        ..Default::default()
    };
    // SAFETY: as in `save_dialog`.
    let ok = unsafe { GetOpenFileNameW(&mut ofn) }.as_bool();
    ok.then(|| path_from_buffer(&buf)).flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialog_strings_are_zero_terminated_utf16() {
        assert_eq!(utf16_z("ab"), [97, 98, 0]);
        let f = filter_spec(&[("A", "*.a"), ("B", "*.b")]);
        let s = String::from_utf16(&f).unwrap();
        assert_eq!(s, "A\0*.a\0B\0*.b\0\0");
        let mut buf = utf16_z("C:\\x\\é.toml");
        buf.extend([0, 7, 7]);
        assert_eq!(path_from_buffer(&buf), Some(PathBuf::from("C:\\x\\é.toml")));
        assert_eq!(path_from_buffer(&[0, 1]), None);
    }
}
