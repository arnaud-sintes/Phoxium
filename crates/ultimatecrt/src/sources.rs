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
    /// Executable file name, `None` when it cannot be queried (see [`executable_name`]).
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

/// Window whose title starts with `needle`, else the first one that contains it (case insensitive), among
/// the windows the picker would list ([`crate::picker::exclusion`]: no cloaked, tool, owned or shell
/// window, review D-17) that are not minimised. Starting with is preferred so a console titled with the
/// program path does not win.
pub fn find_by_title(needle: &str) -> Option<WinInfo> {
    let windows = enumerate();
    let w = find_title_in(&windows, needle)?;
    Some(WinInfo {
        hwnd: HWND(w.hwnd as *mut std::ffi::c_void),
        title: w.title.clone(),
        class: w.class.clone(),
        width: w.width,
        height: w.height,
    })
}

/// The pure part of [`find_by_title`]: the first candidate of `windows` (z-order) whose title starts with
/// `needle`, else the first that contains it, case insensitive; excluded and minimised windows are skipped.
pub fn find_title_in<'a>(
    windows: &'a [crate::picker::RawWindow],
    needle: &str,
) -> Option<&'a crate::picker::RawWindow> {
    let needle = needle.to_lowercase();
    let candidates: Vec<&crate::picker::RawWindow> = windows
        .iter()
        .filter(|w| crate::picker::exclusion(w).is_none() && !w.minimized)
        .collect();
    let title = |w: &&crate::picker::RawWindow| w.title.to_lowercase();
    candidates
        .iter()
        .find(|w| title(w).starts_with(&needle))
        .or_else(|| candidates.iter().find(|w| title(w).contains(&needle)))
        .copied()
}

/// File name (last path component, either separator) of an executable path: `C:\Games\Foo.exe` -> `Foo.exe`.
pub fn exe_file_name(path: &str) -> &str {
    path.trim_end().rsplit(['\\', '/']).next().unwrap_or("")
}

/// Executable file name of the process owning `hwnd`, `None` when it cannot be queried.
/// `PROCESS_QUERY_LIMITED_INFORMATION` usually succeeds even on elevated processes; `None` means a
/// protected process (anti-cheat, protected light processes), a system process, or a process that
/// exited meanwhile. Key of the source profiles (T-51).
pub fn executable_name(hwnd: HWND) -> Option<String> {
    let path = executable_path(hwnd)?;
    let name = exe_file_name(&path);
    (!name.is_empty()).then(|| name.to_string())
}

/// Full executable path of the process owning `hwnd` (see [`executable_name`]).
pub fn executable_path(hwnd: HWND) -> Option<String> {
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
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

// ---------------------------------------------------------------------------------------------------------
// The window picker's enumeration (A23): thin glue, the decisions are `crate::picker`.

unsafe extern "system" fn collect_all(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let list = &mut *(lparam.0 as *mut Vec<HWND>);
    list.push(hwnd);
    true.into()
}

/// The facts of one top-level window for [`crate::picker::exclusion`]. Cheap queries only; the process is opened
/// only for windows that are visible and titled (the others are excluded anyway).
fn raw_window(hwnd: HWND, own_pid: u32, monitors: &[crate::modes::IRect]) -> crate::picker::RawWindow {
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindow, GetWindowLongPtrW, GetWindowPlacement, GWL_EXSTYLE, GW_OWNER, WINDOWPLACEMENT, WS_EX_APPWINDOW,
        WS_EX_TOOLWINDOW,
    };
    let mut w = crate::picker::RawWindow {
        hwnd: hwnd.0 as isize,
        ..Default::default()
    };
    // SAFETY: plain queries on a window handle from EnumWindows; a window destroyed meanwhile makes them fail,
    // which leaves the defaults (invisible, untitled: excluded).
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        w.own_process = pid == own_pid;
        w.visible = IsWindowVisible(hwnd).as_bool();
        let mut buf = [0u16; 512];
        let n = GetWindowTextW(hwnd, &mut buf);
        w.title = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
        let n = GetClassNameW(hwnd, &mut buf);
        w.class = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
        if w.own_process || !w.visible || w.title.trim().is_empty() {
            return w;
        }
        let mut cloaked = 0u32;
        let _ = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            (&mut cloaked as *mut u32).cast(),
            std::mem::size_of::<u32>() as u32,
        );
        w.cloaked = cloaked != 0;
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        w.tool_window = ex & WS_EX_TOOLWINDOW.0 != 0;
        w.app_window = ex & WS_EX_APPWINDOW.0 != 0;
        w.owned = GetWindow(hwnd, GW_OWNER).is_ok_and(|o| !o.is_invalid());
        w.minimized = IsIconic(hwnd).as_bool();
        let mut r = RECT::default();
        if w.minimized {
            // A minimised window's rectangle is the icon's: its restored size is in the placement.
            let mut p = WINDOWPLACEMENT {
                length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
                ..Default::default()
            };
            if GetWindowPlacement(hwnd, &mut p).is_ok() {
                r = p.rcNormalPosition;
            }
        } else {
            let _ = GetWindowRect(hwnd, &mut r);
            let m = crate::output::monitor_of(hwnd);
            w.monitor = monitors.iter().position(|x| *x == m);
        }
        (w.width, w.height) = (r.right - r.left, r.bottom - r.top);
        w.exe_path = executable_path(hwnd);
        w.exe = w
            .exe_path
            .as_deref()
            .map(exe_file_name)
            .filter(|n| !n.is_empty())
            .map(str::to_string);
    }
    w
}

/// Every top-level window with the facts the picker filters on, top of the z-order first (A23).
pub fn enumerate() -> Vec<crate::picker::RawWindow> {
    let mut handles: Vec<HWND> = Vec::new();
    // SAFETY: the callback only pushes into the vector passed as LPARAM, alive for the call.
    unsafe {
        let _ = EnumWindows(Some(collect_all), LPARAM(&mut handles as *mut _ as isize));
    }
    let own = unsafe { GetCurrentProcessId() };
    let monitors = crate::output::monitor_irects();
    handles.into_iter().map(|h| raw_window(h, own, &monitors)).collect()
}

/// The 32 x 32 icon of an executable as RGBA (`PrivateExtractIconsW`, the first icon of the file), `None` when the
/// file has none or it cannot be read (the picker then draws a placeholder).
pub fn exe_icon(path: &str) -> Option<(u32, u32, Vec<u8>)> {
    use windows::Win32::Graphics::Gdi::{
        DeleteObject, GetDC, GetDIBits, ReleaseDC, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HGDIOBJ,
    };
    use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, PrivateExtractIconsW, HICON, ICONINFO};
    const SIZE: i32 = 32;
    let wide: Vec<u16> = path.encode_utf16().chain([0]).collect();
    let mut name = [0u16; 260];
    let n = wide.len().min(name.len() - 1);
    name[..n].copy_from_slice(&wide[..n]);
    let mut icons = [HICON::default()];
    // SAFETY: a NUL-terminated path in a MAX_PATH buffer, one output slot; the icon is destroyed below.
    let got = unsafe { PrivateExtractIconsW(&name, 0, SIZE, SIZE, Some(&mut icons), None, 0) };
    let icon = icons[0];
    if got == 0 || got == u32::MAX || icon.is_invalid() {
        return None;
    }
    // SAFETY: GDI reads of the icon's bitmaps into local buffers of the right size; every object created
    // here (the bitmaps of GetIconInfo, the DC, the icon) is released before returning.
    unsafe {
        let mut info = ICONINFO::default();
        let ok = GetIconInfo(icon, &mut info).is_ok();
        let mut result = None;
        if ok && !info.hbmColor.is_invalid() {
            let dc = GetDC(None);
            let mut bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: SIZE,
                    biHeight: -SIZE, // top-down
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bgra = vec![0u8; (SIZE * SIZE * 4) as usize];
            let lines = GetDIBits(
                dc,
                info.hbmColor,
                0,
                SIZE as u32,
                Some(bgra.as_mut_ptr().cast()),
                &mut bmi,
                DIB_RGB_COLORS,
            );
            if lines == SIZE {
                // Icons without an alpha channel: opaque where the AND mask is clear.
                let has_alpha = bgra.as_chunks::<4>().0.iter().any(|p| p[3] != 0);
                let mut mask = vec![0u8; (SIZE * SIZE * 4) as usize];
                if !has_alpha && !info.hbmMask.is_invalid() {
                    let mut mbmi = bmi;
                    let _ = GetDIBits(
                        dc,
                        info.hbmMask,
                        0,
                        SIZE as u32,
                        Some(mask.as_mut_ptr().cast()),
                        &mut mbmi,
                        DIB_RGB_COLORS,
                    );
                }
                let rgba: Vec<u8> = bgra
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .zip(mask.as_chunks::<4>().0)
                    .flat_map(|(p, m)| {
                        let a = if has_alpha {
                            p[3]
                        } else if m[0] == 0 {
                            255
                        } else {
                            0
                        };
                        [p[2], p[1], p[0], a]
                    })
                    .collect();
                result = Some((SIZE as u32, SIZE as u32, rgba));
            }
            ReleaseDC(None, dc);
        }
        if !info.hbmColor.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(info.hbmColor.0));
        }
        if !info.hbmMask.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(info.hbmMask.0));
        }
        let _ = DestroyIcon(icon);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::{exe_file_name, find_title_in};
    use crate::picker::RawWindow;

    fn window(hwnd: isize, title: &str) -> RawWindow {
        RawWindow {
            hwnd,
            title: title.into(),
            class: "GameClass".into(),
            exe: Some("game.exe".into()),
            visible: true,
            width: 640,
            height: 480,
            ..Default::default()
        }
    }

    /// Review D-17: `--window` skips the windows the picker excludes (cloaked on another virtual desktop, a
    /// tool window, our own) and minimised ones; starting with wins over containing.
    #[test]
    fn window_option_skips_excluded_windows() {
        let cloaked = RawWindow {
            cloaked: true,
            ..window(1, "Pixel demo")
        };
        let tool = RawWindow {
            tool_window: true,
            ..window(2, "Pixel demo tools")
        };
        let own = RawWindow {
            own_process: true,
            ..window(3, "Pixel demo")
        };
        let minimised = RawWindow {
            minimized: true,
            ..window(4, "Pixel demo")
        };
        let contains = window(5, "The pixel demo");
        let starts = window(6, "Pixel demo");
        let all = [cloaked, tool, own, minimised, contains.clone(), starts];
        assert_eq!(find_title_in(&all, "pixel").map(|w| w.hwnd), Some(6));
        assert_eq!(find_title_in(&all[..5], "PIXEL").map(|w| w.hwnd), Some(5));
        assert_eq!(find_title_in(&all[..4], "pixel"), None);
        assert_eq!(find_title_in(&[contains], "nothing"), None);
    }

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
