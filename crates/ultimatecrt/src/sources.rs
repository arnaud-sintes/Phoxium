//! Finds the window to capture.

use windows::{
    core::BOOL,
    Win32::{
        Foundation::{HWND, LPARAM, RECT},
        System::Threading::GetCurrentProcessId,
        UI::WindowsAndMessaging::{
            EnumWindows, GetClassNameW, GetWindowRect, GetWindowTextW, GetWindowThreadProcessId, IsIconic,
            IsWindowVisible,
        },
    },
};

pub struct WinInfo {
    pub hwnd: HWND,
    pub title: String,
    pub class: String,
    pub width: i32,
    pub height: i32,
}

unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let list = &mut *(lparam.0 as *mut Vec<WinInfo>);
    if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
        return true.into();
    }
    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    if pid == GetCurrentProcessId() {
        return true.into();
    }
    let mut buf = [0u16; 256];
    let n = GetWindowTextW(hwnd, &mut buf);
    if n == 0 {
        return true.into();
    }
    let title = String::from_utf16_lossy(&buf[..n as usize]);
    let n = GetClassNameW(hwnd, &mut buf);
    let class = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
    let mut r = RECT::default();
    let _ = GetWindowRect(hwnd, &mut r);
    list.push(WinInfo { hwnd, title, class, width: r.right - r.left, height: r.bottom - r.top });
    true.into()
}

/// Visible, non-minimised, titled top-level windows of other processes.
pub fn list_windows() -> Vec<WinInfo> {
    let mut list: Vec<WinInfo> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(collect), LPARAM(&mut list as *mut _ as isize));
    }
    list
}

/// Window whose title starts with `needle`, else the first one that contains it (case insensitive).
/// Starting with is preferred so a console titled with the program path does not win.
pub fn find_by_title(needle: &str) -> Option<WinInfo> {
    let needle = needle.to_lowercase();
    let mut windows = list_windows();
    let pos = windows
        .iter()
        .position(|w| w.title.to_lowercase().starts_with(&needle))
        .or_else(|| windows.iter().position(|w| w.title.to_lowercase().contains(&needle)))?;
    Some(windows.swap_remove(pos))
}
