//! Finds the window to capture.

use windows::{
    core::{BOOL, PWSTR},
    Win32::{
        Foundation::{CloseHandle, HWND, LPARAM, RECT},
        System::Threading::{
            GetCurrentProcessId, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
            PROCESS_QUERY_LIMITED_INFORMATION,
        },
        UI::WindowsAndMessaging::{
            EnumWindows, GetClassNameW, GetWindowRect, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindow,
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
    list.push(WinInfo {
        hwnd,
        title,
        class,
        width: r.right - r.left,
        height: r.bottom - r.top,
    });
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

/// One row of the window list shown by the UI (T-56): plain data, no OS handle types, so the list
/// can be filtered, matched and tested without windows. `hwnd` is the raw handle value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowEntry {
    pub hwnd: isize,
    pub title: String,
    /// Executable file name, `None` when it cannot be queried (for example an elevated process).
    pub exe: Option<String>,
    pub class: String,
    pub width: i32,
    pub height: i32,
}

impl WindowEntry {
    pub fn from_info(info: &WinInfo) -> Self {
        WindowEntry {
            hwnd: info.hwnd.0 as isize,
            title: info.title.clone(),
            exe: executable_name(info.hwnd),
            class: info.class.clone(),
            width: info.width,
            height: info.height,
        }
    }
}

/// The window list for the UI: title, executable and class of every candidate window.
pub fn list_entries() -> Vec<WindowEntry> {
    list_windows().iter().map(WindowEntry::from_info).collect()
}

/// True while `hwnd` still designates a window (a destroyed window or a stale handle gives false).
pub fn window_exists(hwnd: isize) -> bool {
    unsafe { IsWindow(Some(HWND(hwnd as *mut std::ffi::c_void))).as_bool() }
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

/// File name (last path component, either separator) of an executable path: `C:\Games\Foo.exe` -> `Foo.exe`.
pub fn exe_file_name(path: &str) -> &str {
    path.trim_end().rsplit(['\\', '/']).next().unwrap_or("")
}

/// Executable file name of the process owning `hwnd`, `None` when it cannot be queried (for example an
/// elevated process). Key of the source profiles (T-51).
pub fn executable_name(hwnd: HWND) -> Option<String> {
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return None;
        }
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let r = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(process);
        r.ok()?;
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        let name = exe_file_name(&path);
        (!name.is_empty()).then(|| name.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::exe_file_name;

    #[test]
    fn parses_executable_names() {
        assert_eq!(exe_file_name(r"C:\Games\Mina\Mina.exe"), "Mina.exe");
        assert_eq!(exe_file_name("D:/x/y/foo.EXE"), "foo.EXE");
        assert_eq!(exe_file_name("bare.exe"), "bare.exe");
        assert_eq!(exe_file_name(r"C:\Program Files (x86)\A B\c d.exe"), "c d.exe");
        assert_eq!(exe_file_name(r"C:\dir\"), "");
        assert_eq!(exe_file_name(""), "");
    }
}
