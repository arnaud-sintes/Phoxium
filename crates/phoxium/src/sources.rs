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
        let hwnd = info.hwnd.0 as isize;
        WindowEntry {
            hwnd,
            title: info.title.clone(),
            // A whole monitor has no window to ask: its program is the pseudo-program of the picker's rows, so
            // the saved profile and the last choice are keyed the same however the source was found.
            exe: if monitor_index(hwnd).is_some() {
                Some(MONITOR_EXE.to_string())
            } else {
                executable_name(info.hwnd)
            },
            class: info.class.clone(),
            width: info.width,
            height: info.height,
        }
    }
}

/// The window list for the UI and for the reattach: title, executable and class of every candidate window, then
/// the connected monitors as pseudo-windows (a whole-monitor source that was lost is found again when its
/// monitor comes back).
pub fn list_entries() -> Vec<WindowEntry> {
    let mut entries: Vec<WindowEntry> = list_windows().iter().map(WindowEntry::from_info).collect();
    entries.extend(monitor_entries(&crate::output::monitor_irects()));
    entries
}

/// The monitors as list entries (title "Whole screen N", program [`MONITOR_EXE`], class with the monitor number,
/// handle [`monitor_handle`]).
pub fn monitor_entries(monitors: &[crate::modes::IRect]) -> Vec<WindowEntry> {
    raw_monitors(monitors)
        .into_iter()
        .map(|w| WindowEntry {
            hwnd: w.hwnd,
            title: w.title,
            exe: w.exe,
            class: w.class,
            width: w.width,
            height: w.height,
        })
        .collect()
}

/// True while `hwnd` still designates a window (a destroyed window or a stale handle gives false), or, for the
/// handle of a whole monitor ([`monitor_handle`]), while that monitor is connected.
pub fn window_exists(hwnd: isize) -> bool {
    if let Some(i) = monitor_index(hwnd) {
        return i < crate::output::monitor_irects().len();
    }
    unsafe { IsWindow(Some(HWND(hwnd as *mut std::ffi::c_void))).as_bool() }
}

/// Whole monitors as capture sources: each connected monitor is listed with the windows, as a pseudo-window of
/// the program [`MONITOR_EXE`], the class [`MONITOR_CLASS_PREFIX`] and its number, the title "Whole screen N" and the
/// handle [`monitor_handle`] (negative: no window has one). Everything keyed by window (the picker, the last
/// choice, the reattach after the monitor comes back, the saved profile of a program and class) then works for
/// a monitor without a case of its own; only the capture start and the places that call Win32 with the handle
/// know the difference.
pub const MONITOR_EXE: &str = "Display";
/// Class prefix of the monitor pseudo-windows (followed by the 1-based monitor number).
pub const MONITOR_CLASS_PREFIX: &str = "phoxium_monitor_";

/// The handle standing for monitor `index` (0-based, the order of `output::monitor_rects`): -1, -2, ...
pub fn monitor_handle(index: usize) -> isize {
    -(index as isize) - 1
}

/// The monitor index of a handle made by [`monitor_handle`], `None` for a window handle.
pub fn monitor_index(handle: isize) -> Option<usize> {
    (handle < 0).then(|| (-(handle + 1)) as usize)
}

/// The pseudo-windows of the connected monitors, for the window list.
pub fn raw_monitors(monitors: &[crate::modes::IRect]) -> Vec<crate::picker::RawWindow> {
    monitors
        .iter()
        .enumerate()
        .map(|(i, m)| crate::picker::RawWindow {
            hwnd: monitor_handle(i),
            title: format!("Whole screen {}", i + 1),
            class: format!("{MONITOR_CLASS_PREFIX}{}", i + 1),
            exe: Some(MONITOR_EXE.to_string()),
            exe_path: None,
            visible: true,
            width: m.width(),
            height: m.height(),
            monitor: Some(i),
            ..Default::default()
        })
        .collect()
}

/// Window whose title starts with `needle`, else the first one that contains it (case insensitive), among
/// the windows the picker would list ([`crate::picker::exclusion`]: no cloaked, tool, owned or shell
/// window, review D-17) that are not minimised. Starting with is preferred so a console titled with the
/// program path does not win.
pub fn find_by_title(needle: &str) -> Option<WinInfo> {
    let windows = enumerate();
    let w = find_title_in(&windows, needle)?;
    Some(win_info(w))
}

fn win_info(w: &crate::picker::RawWindow) -> WinInfo {
    WinInfo {
        hwnd: HWND(w.hwnd as *mut std::ffi::c_void),
        title: w.title.clone(),
        class: w.class.clone(),
        width: w.width,
        height: w.height,
    }
}

/// Whether the executable file name `exe` is the program `wanted`, case insensitive, with or without `.exe`:
/// `Game.exe` matches `game` and `GAME.EXE`.
pub fn exe_matches(exe: &str, wanted: &str) -> bool {
    let wanted = wanted.trim().to_lowercase();
    let exe = exe.trim().to_lowercase();
    !wanted.is_empty() && (exe == wanted || exe == format!("{wanted}.exe"))
}

/// The window of the program `process` (see [`exe_matches`]) among `windows`, optionally also with `title` in its title
/// (starting with it preferred, as [`find_title_in`]): among the windows the picker would list that are not
/// minimised, the largest one (a game has its main window and often a launcher or a splash of the same program),
/// the first in z-order on a tie.
pub fn find_process_in<'a>(
    windows: &'a [crate::picker::RawWindow],
    process: &str,
    title: Option<&str>,
) -> Option<&'a crate::picker::RawWindow> {
    let mut candidates: Vec<&crate::picker::RawWindow> = windows
        .iter()
        .filter(|w| crate::picker::exclusion(w).is_none() && !w.minimized)
        .filter(|w| w.exe.as_deref().is_some_and(|e| exe_matches(e, process)))
        .collect();
    if let Some(needle) = title.map(str::to_lowercase) {
        let starts: Vec<_> = candidates
            .iter()
            .copied()
            .filter(|w| w.title.to_lowercase().starts_with(&needle))
            .collect();
        candidates = if starts.is_empty() {
            candidates
                .into_iter()
                .filter(|w| w.title.to_lowercase().contains(&needle))
                .collect()
        } else {
            starts
        };
    }
    let area = |w: &&crate::picker::RawWindow| i64::from(w.width.max(0)) * i64::from(w.height.max(0));
    let best = candidates.iter().map(area).max()?;
    candidates.into_iter().find(|w| area(w) == best)
}

/// The window the command line asks for: of the program `process` (and with `title` in its title when both are
/// given), else with `title` in its title ([`find_by_title`]).
pub fn find_target(process: Option<&str>, title: Option<&str>) -> Option<WinInfo> {
    match process {
        Some(p) => {
            let windows = enumerate();
            find_process_in(&windows, p, title).map(win_info)
        }
        None => find_by_title(title?),
    }
}

/// Calls `find` until it returns something: at once with `wait_s` of `None`, else every 250 ms until `wait_s` seconds
/// have passed (`0`: no limit). `elapsed` is the seconds since the start (the clock, given for the tests) and `sleep`
/// the pause between two tries. `None` when the wait ran out.
pub fn wait_for_window<T>(
    wait_s: Option<f64>,
    mut find: impl FnMut() -> Option<T>,
    mut sleep: impl FnMut(std::time::Duration),
    mut elapsed: impl FnMut() -> f64,
) -> Option<T> {
    loop {
        if let Some(found) = find() {
            return Some(found);
        }
        let limit = wait_s?;
        if limit > 0.0 && elapsed() >= limit {
            return None;
        }
        sleep(std::time::Duration::from_millis(250));
    }
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

/// Whether the attached window `hwnd` can deliver frames now: minimised (`IsIconic`) or cloaked by the compositor
/// (`DWMWA_CLOAKED`: another virtual desktop). Two cheap queries, made about once a second.
pub fn visibility(hwnd: HWND) -> crate::lifecycle::SourceVisibility {
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
    // SAFETY: plain queries on a window handle; a destroyed window makes them fail, which reads as visible (the
    // lost-source path handles a closed window).
    unsafe {
        let mut cloaked = 0u32;
        let _ = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            (&mut cloaked as *mut u32).cast(),
            std::mem::size_of::<u32>() as u32,
        );
        crate::lifecycle::SourceVisibility::of(IsIconic(hwnd).as_bool(), cloaked != 0)
    }
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
    let mut windows: Vec<crate::picker::RawWindow> =
        handles.into_iter().map(|h| raw_window(h, own, &monitors)).collect();
    // The whole monitors come after the windows (the picker lists them last).
    windows.extend(raw_monitors(&monitors));
    windows
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
    use super::{
        exe_file_name, exe_matches, find_process_in, find_title_in, monitor_handle, monitor_index, raw_monitors,
        wait_for_window,
    };
    use crate::picker::RawWindow;

    /// A whole monitor has a negative handle (no window has one) and comes back as its index; a window handle is
    /// not a monitor.
    #[test]
    fn monitor_handles_round_trip_and_are_never_window_handles() {
        for i in [0usize, 1, 2, 7] {
            let h = monitor_handle(i);
            assert!(h < 0);
            assert_eq!(monitor_index(h), Some(i));
        }
        assert_eq!(monitor_handle(0), -1);
        assert_eq!(monitor_index(0), None);
        assert_eq!(monitor_index(0x10ABC), None);
    }

    /// The monitors are listed like windows: the picker takes them (title "Whole screen N", program "Display"), the
    /// last choice and the saved profile key them by their class, which holds the monitor number, so a profile is
    /// per monitor; `--window "Whole screen 2"` finds one.
    #[test]
    fn monitors_are_picker_rows_with_a_class_per_monitor() {
        let monitors = [
            crate::modes::IRect::new(0, 0, 2560, 1440),
            crate::modes::IRect::new(2560, -251, 4000, 2309),
        ];
        let raw = raw_monitors(&monitors);
        assert!(
            raw.iter().all(|w| crate::picker::exclusion(w).is_none()),
            "listed by the picker"
        );
        let rows = crate::picker::candidates(&raw);
        assert_eq!(rows.len(), 2);
        assert_eq!(
            (
                rows[1].title.as_str(),
                rows[1].class.as_str(),
                rows[1].program().as_str()
            ),
            ("Whole screen 2", "phoxium_monitor_2", "Display")
        );
        assert_eq!((rows[1].width, rows[1].height, rows[1].monitor), (1440, 2560, Some(1)));
        assert_eq!(
            rows[1].details(),
            "Display  \u{00B7}  1440 \u{00D7} 2560  \u{00B7}  monitor 2"
        );
        assert_eq!(rows[0].last_source().class, "phoxium_monitor_1");
        assert_eq!(
            find_title_in(&raw, "whole screen 2").map(|w| w.hwnd),
            Some(monitor_handle(1))
        );
    }

    /// The reattach list holds the monitors too (a whole-monitor source that was lost is found when its monitor comes
    /// back), and a monitor found by title keeps its identity: the pseudo-program "Display" and its class.
    #[test]
    fn monitors_are_in_the_reattach_list_and_keep_their_identity() {
        let monitors = [
            crate::modes::IRect::new(0, 0, 2560, 1440),
            crate::modes::IRect::new(2560, 0, 4000, 2560),
        ];
        let entries = super::monitor_entries(&monitors);
        assert_eq!(entries.len(), 2);
        assert_eq!(
            (entries[1].hwnd, entries[1].exe.as_deref(), entries[1].class.as_str()),
            (monitor_handle(1), Some("Display"), "phoxium_monitor_2")
        );
        // The target of a lost "Whole screen 2" matches the entry of the same monitor, not the other one.
        let target = crate::lifecycle::TargetKey::of(&entries[1]);
        let known = std::collections::HashSet::new();
        let found = crate::lifecycle::choose_reattach(&target, monitor_handle(1), &known, &entries);
        assert_eq!(found.map(|e| e.hwnd), Some(monitor_handle(1)));
        let other = crate::lifecycle::TargetKey::of(&entries[0]);
        assert_eq!(
            crate::lifecycle::choose_reattach(&other, monitor_handle(1), &known, &entries).map(|e| e.hwnd),
            Some(monitor_handle(0))
        );
        // `from_info` of a monitor handle gives the pseudo-program without asking Windows about the handle.
        let info = super::WinInfo {
            hwnd: windows::Win32::Foundation::HWND(monitor_handle(1) as *mut std::ffi::c_void),
            title: "Whole screen 2".into(),
            class: "phoxium_monitor_2".into(),
            width: 1440,
            height: 2560,
        };
        assert_eq!(super::WindowEntry::from_info(&info).exe.as_deref(), Some("Display"));
    }

    /// A listable window of `exe` with a size.
    fn of_exe(hwnd: isize, title: &str, exe: &str, size: (i32, i32)) -> RawWindow {
        RawWindow {
            exe: Some(exe.to_string()),
            width: size.0,
            height: size.1,
            ..window(hwnd, title)
        }
    }

    #[test]
    fn a_program_name_matches_with_or_without_exe_in_any_case() {
        assert!(exe_matches("Game.exe", "game"));
        assert!(exe_matches("Game.exe", "GAME.EXE"));
        assert!(exe_matches("game", "game"));
        assert!(!exe_matches("Game.exe", "gam"));
        assert!(!exe_matches("Game.exe", ""));
        assert!(!exe_matches("Game2.exe", "game"));
    }

    #[test]
    fn the_largest_window_of_the_program_is_taken() {
        let list = [
            of_exe(1, "Launcher", "Game.exe", (400, 300)),
            of_exe(2, "Other", "Other.exe", (3000, 2000)),
            of_exe(3, "Game", "Game.exe", (1280, 720)),
            of_exe(4, "Game again", "game.exe", (1280, 720)),
        ];
        assert_eq!(
            find_process_in(&list, "game", None).map(|w| w.hwnd),
            Some(3),
            "largest, first on a tie"
        );
        assert_eq!(find_process_in(&list, "nothing", None), None);
        // The title narrows it: starting with it first, else containing it.
        assert_eq!(find_process_in(&list, "game", Some("launch")).map(|w| w.hwnd), Some(1));
        assert_eq!(find_process_in(&list, "game", Some("again")).map(|w| w.hwnd), Some(4));
        assert_eq!(find_process_in(&list, "game", Some("zzz")), None);
    }

    #[test]
    fn minimised_and_unlistable_windows_of_the_program_are_skipped() {
        let mut small = of_exe(1, "Game", "Game.exe", (800, 600));
        let mut big = of_exe(2, "Game big", "Game.exe", (1920, 1080));
        big.minimized = true;
        let mut unlisted = of_exe(3, "Game tool", "Game.exe", (2000, 2000));
        unlisted.tool_window = true;
        small.exe_path = None;
        let list = [big, unlisted, small];
        assert_eq!(find_process_in(&list, "game", None).map(|w| w.hwnd), Some(1));
        // A window whose program cannot be queried never matches a program name.
        let mut none = of_exe(4, "Game", "Game.exe", (800, 600));
        none.exe = None;
        assert_eq!(find_process_in(&[none], "game", None), None);
    }

    #[test]
    fn waiting_for_a_window_stops_at_the_limit_or_when_it_appears() {
        let (clock, tries) = (std::cell::Cell::new(0.0f64), std::cell::Cell::new(0));
        let sleep = |d: std::time::Duration| clock.set(clock.get() + d.as_secs_f64());
        // No wait asked: one try.
        let found: Option<u32> = wait_for_window(
            None,
            || {
                tries.set(tries.get() + 1);
                None
            },
            sleep,
            || clock.get(),
        );
        assert_eq!((found, tries.get()), (None, 1));
        // A wait of 1 s: tries every 250 ms, gives up after 1 s.
        tries.set(0);
        clock.set(0.0);
        let found: Option<u32> = wait_for_window(
            Some(1.0),
            || {
                tries.set(tries.get() + 1);
                None
            },
            sleep,
            || clock.get(),
        );
        assert_eq!((found, tries.get()), (None, 5));
        // The window appears at the third try.
        tries.set(0);
        clock.set(0.0);
        let found = wait_for_window(
            Some(10.0),
            || {
                tries.set(tries.get() + 1);
                (tries.get() == 3).then_some(7u32)
            },
            sleep,
            || clock.get(),
        );
        assert_eq!((found, tries.get()), (Some(7), 3));
        // 0 means no limit: it waits until the window appears.
        tries.set(0);
        clock.set(0.0);
        let found = wait_for_window(
            Some(0.0),
            || {
                tries.set(tries.get() + 1);
                (tries.get() == 50).then_some(1u32)
            },
            sleep,
            || clock.get(),
        );
        assert_eq!((found, tries.get()), (Some(1), 50));
    }

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
