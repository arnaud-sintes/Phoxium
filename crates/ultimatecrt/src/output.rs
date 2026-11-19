//! The output window (raw Win32).
//!
//! Two modes:
//! - windowed: a normal resizable window, Esc closes it;
//! - fullscreen: borderless, topmost, non-activating, covering one monitor, never takes focus.
//!
//! Ctrl+Alt+Q is a global hotkey that always quits, since a non-activating fullscreen window
//! cannot receive keyboard input.
//!
//! Hand-back (T-07): `WS_EX_NOACTIVATE`, `SW_SHOWNOACTIVATE`, `MA_NOACTIVATE`, `SWP_NOACTIVATE` and a
//! disabled window do not stop Windows from activating the window once, about 25 s after the
//! process starts (initiator outside the program; it happens with no Vulkan and no capture, see
//! QUESTIONS OUT-2). A non-activating output therefore hands the foreground back at once when it is
//! activated: to the window it came from, else the last other foreground window sampled by [`pump`],
//! else the source window. `WM_ACTIVATE` is answered without `DefWindowProc`, so the keyboard focus
//! never moves to the output; what remains is a foreground blip of about 0.1 s.
//!
//! Settings menu (T-52a, D-7): while it is open ([`set_menu_open`]) the output may be activated (no
//! `WS_EX_NOACTIVATE`, no hand-back: the menu took the foreground on purpose), its input messages are queued
//! for the UI ([`take_ui_messages`], `crate::ui::input::routed`), the mouse is captured during a button press
//! and the cursor shape is the one ImGui asks for ([`set_menu_cursor`]).

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};

use windows::{
    core::{w, Result, HSTRING, PCWSTR},
    Win32::{
        Foundation::{E_INVALIDARG, HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::Gdi::{
            EnumDisplayMonitors, GetMonitorInfoW, MonitorFromWindow, HDC, HMONITOR, MONITORINFO,
            MONITOR_DEFAULTTONEAREST,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Input::KeyboardAndMouse::{
                GetCapture, RegisterHotKey, ReleaseCapture, SetCapture, TrackMouseEvent, UnregisterHotKey, MOD_ALT,
                MOD_CONTROL, MOD_NOREPEAT, TME_LEAVE, TRACKMOUSEEVENT,
            },
            WindowsAndMessaging::*,
        },
    },
};

use crate::ui::input as ui_input;

/// The settings menu is open (T-52a): input goes to the UI and the output may be active.
static MENU_OPEN: AtomicBool = AtomicBool::new(false);
/// `NO_ACTIVATE` before the menu opened, restored when it closes.
static MENU_SAVED_NO_ACTIVATE: AtomicBool = AtomicBool::new(false);
/// System cursor (`IDC_*` number) shown over the client area while the menu is open; 0 hides it.
static MENU_CURSOR: AtomicU32 = AtomicU32::new(32512);
/// `WM_MOUSELEAVE` is requested (`TrackMouseEvent`) for the current stay of the mouse over the window.
static MOUSE_TRACKED: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// Input messages of the output window for the UI (message, `WPARAM`, `LPARAM`), queued by the window
    /// procedure while the menu is open (and DPI changes always), taken by the app every frame.
    static UI_MESSAGES: RefCell<Vec<(u32, usize, isize)>> = const { RefCell::new(Vec::new()) };
}

/// Opens or closes the menu mode of the output window `hwnd` (T-52a, called by `crate::ui::menu::Menu`).
/// Open: `WS_EX_NOACTIVATE` off and the T-07 hand-back off (the menu takes the foreground on purpose, the
/// hand-back would undo it), input queued for the UI. Close: queueing stops, `WS_EX_NOACTIVATE` and the
/// hand-back are restored as they were. The caller moves the foreground afterwards.
pub fn set_menu_open(hwnd: HWND, open: bool) {
    if MENU_OPEN.load(Ordering::Relaxed) == open {
        return;
    }
    let no_activate = if open {
        MENU_SAVED_NO_ACTIVATE.store(NO_ACTIVATE.load(Ordering::Relaxed), Ordering::Relaxed);
        false
    } else {
        MENU_SAVED_NO_ACTIVATE.load(Ordering::Relaxed)
    };
    MENU_OPEN.store(open, Ordering::Relaxed);
    NO_ACTIVATE.store(no_activate, Ordering::Relaxed);
    MOUSE_TRACKED.store(false, Ordering::Relaxed);
    if !open {
        UI_MESSAGES.with(|q| q.borrow_mut().clear());
    }
    // SAFETY: our own window; only the WS_EX_NOACTIVATE bit of the extended style changes.
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let bit = WS_EX_NOACTIVATE.0 as isize;
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, if no_activate { ex | bit } else { ex & !bit });
        if !open && GetCapture() == hwnd {
            let _ = ReleaseCapture();
        }
    }
}

/// The input messages queued for the UI since the last call (see [`set_menu_open`]).
pub fn take_ui_messages() -> Vec<(u32, usize, isize)> {
    UI_MESSAGES.with(|q| std::mem::take(&mut *q.borrow_mut()))
}

/// The cursor shown over the client area while the menu is open: a system cursor (`IDC_*` number), or
/// `None` to hide it (ImGui's choice, `crate::ui::input::cursor_id`).
pub fn set_menu_cursor(id: Option<u16>) {
    MENU_CURSOR.store(id.map_or(0, u32::from), Ordering::Relaxed);
}

/// The menu part of the window procedure: queues the routed messages for the UI, captures the mouse while
/// a button is down, sets the cursor. `Some` when the message is answered here.
fn menu_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    let open = MENU_OPEN.load(Ordering::Relaxed);
    if !(open && ui_input::routed(msg) || msg == ui_input::WM_DPICHANGED) {
        if open && msg == WM_SETCURSOR && lparam.0 & 0xFFFF == ui_input::HTCLIENT {
            let id = MENU_CURSOR.load(Ordering::Relaxed);
            // SAFETY: system cursors; a null cursor hides it.
            unsafe {
                let cursor = (id != 0)
                    .then(|| LoadCursorW(None, PCWSTR(id as usize as *const u16)).ok())
                    .flatten();
                SetCursor(cursor);
            }
            return Some(LRESULT(1));
        }
        return None;
    }
    UI_MESSAGES.with(|q| q.borrow_mut().push((msg, wparam.0, lparam.0)));
    // SAFETY: mouse capture and leave tracking of our own window; the capture is released on the button up or
    // when the menu closes.
    unsafe {
        match msg {
            // ImGui forgets the mouse when it leaves the window (hover state), which needs WM_MOUSELEAVE.
            WM_MOUSEMOVE if !MOUSE_TRACKED.swap(true, Ordering::Relaxed) => {
                let mut t = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                let _ = TrackMouseEvent(&mut t);
            }
            ui_input::WM_MOUSELEAVE => MOUSE_TRACKED.store(false, Ordering::Relaxed),
            WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN => {
                SetCapture(hwnd);
            }
            WM_LBUTTONUP | WM_RBUTTONUP | WM_MBUTTONUP | WM_XBUTTONUP if GetCapture() == hwnd => {
                let _ = ReleaseCapture();
            }
            _ => {}
        }
    }
    ui_input::consumes(msg).then_some(LRESULT(0))
}

static NO_ACTIVATE: AtomicBool = AtomicBool::new(false);
/// The output window, 0 before it exists.
static OWN: AtomicIsize = AtomicIsize::new(0);
/// The source window (`Options::follow`), 0 if none.
static SOURCE: AtomicIsize = AtomicIsize::new(0);
/// The last foreground window that was not the output, sampled at every [`pump`].
static LAST_FOREGROUND: AtomicIsize = AtomicIsize::new(0);
/// Activations handed back so far (reported by [`take_handbacks`]).
static HANDBACKS: AtomicU32 = AtomicU32::new(0);

/// Where a non-activating output sends the foreground when Windows activates it anyway: the window
/// the activation came from (`from`, from `WM_ACTIVATE`, often 0 across threads), else the last
/// foreground window seen that was not the output, else the source window. `None` when the output
/// may be active (activating mode, or a deactivation) or no candidate is known. Pure.
pub fn hand_back_target(
    no_activate: bool,
    activated: bool,
    own: isize,
    from: isize,
    last: isize,
    source: isize,
) -> Option<isize> {
    if !no_activate || !activated {
        return None;
    }
    [from, last, source].into_iter().find(|&h| h != 0 && h != own)
}

/// Activations handed back since the last call (for a console line).
pub fn take_handbacks() -> u32 {
    HANDBACKS.swap(0, Ordering::Relaxed)
}

pub struct Options {
    pub fullscreen: bool,
    pub no_activate: bool,
    /// Index into the monitor list, or the monitor nearest to `follow` if `None`.
    pub monitor: Option<usize>,
    /// Source window, used to pick the monitor to cover.
    pub follow: HWND,
    /// Client size of the window in windowed mode.
    pub client_size: (i32, i32),
}

pub struct Output {
    pub hwnd: HWND,
    pub hinstance: isize,
}

unsafe extern "system" fn monitor_proc(m: HMONITOR, _: HDC, _: *mut RECT, lparam: LPARAM) -> windows::core::BOOL {
    let list = &mut *(lparam.0 as *mut Vec<HMONITOR>);
    list.push(m);
    true.into()
}

pub fn monitor_rects() -> Vec<RECT> {
    let mut list: Vec<HMONITOR> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(monitor_proc), LPARAM(&mut list as *mut _ as isize));
    }
    list.into_iter().filter_map(monitor_rect).collect()
}

fn monitor_rect(m: HMONITOR) -> Option<RECT> {
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe { GetMonitorInfoW(m, &mut info).as_bool().then_some(info.rcMonitor) }
}

pub fn create(opts: &Options) -> Result<Output> {
    NO_ACTIVATE.store(opts.no_activate || opts.fullscreen, Ordering::Relaxed);
    SOURCE.store(opts.follow.0 as isize, Ordering::Relaxed);
    LAST_FOREGROUND.store(unsafe { GetForegroundWindow() }.0 as isize, Ordering::Relaxed);
    unsafe {
        let hinstance = GetModuleHandleW(None)?;
        let class = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: w!("ultimatecrt_output"),
            ..Default::default()
        };
        RegisterClassW(&class);

        let hwnd = if opts.fullscreen {
            let rect = match opts.monitor {
                Some(i) => monitor_rects().get(i).copied(),
                None => monitor_rect(MonitorFromWindow(opts.follow, MONITOR_DEFAULTTONEAREST)),
            }
            .ok_or_else(|| {
                windows::core::Error::new(
                    E_INVALIDARG,
                    "no such monitor (see the monitor lines of --list-windows)",
                )
            })?;
            CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                w!("ultimatecrt_output"),
                &HSTRING::from("ultimatecrt"),
                WS_POPUP,
                rect.left,
                rect.top,
                rect.right - rect.left,
                rect.bottom - rect.top,
                None,
                None,
                Some(hinstance.into()),
                None,
            )?
        } else {
            let style = WS_OVERLAPPEDWINDOW;
            let mut r = RECT {
                left: 0,
                top: 0,
                right: opts.client_size.0,
                bottom: opts.client_size.1,
            };
            let _ = AdjustWindowRectEx(&mut r, style, false, WINDOW_EX_STYLE(0));
            let ex = if opts.no_activate {
                WS_EX_NOACTIVATE
            } else {
                WINDOW_EX_STYLE(0)
            };
            CreateWindowExW(
                ex,
                w!("ultimatecrt_output"),
                &HSTRING::from("ultimatecrt"),
                style,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                r.right - r.left,
                r.bottom - r.top,
                None,
                None,
                Some(hinstance.into()),
                None,
            )?
        };

        if let Err(e) = RegisterHotKey(Some(hwnd), 1, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, 'Q' as u32) {
            if opts.fullscreen {
                // Without the hotkey a fullscreen output that cannot take focus has no keyboard exit.
                let _ = DestroyWindow(hwnd);
                return Err(windows::core::Error::new(
                    e.code(),
                    "cannot register the Ctrl+Alt+Q quit hotkey (another program owns it); refusing to go fullscreen",
                ));
            }
            eprintln!("warning: cannot register Ctrl+Alt+Q: {e}");
        }
        OWN.store(hwnd.0 as isize, Ordering::Relaxed);
        let _ = ShowWindow(
            hwnd,
            if NO_ACTIVATE.load(Ordering::Relaxed) {
                SW_SHOWNOACTIVATE
            } else {
                SW_SHOW
            },
        );
        Ok(Output {
            hwnd,
            hinstance: hinstance.0 as isize,
        })
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        unsafe {
            let _ = UnregisterHotKey(Some(self.hwnd), 1);
            if IsWindow(Some(self.hwnd)).as_bool() {
                let _ = DestroyWindow(self.hwnd);
            }
        }
    }
}

/// Turns the fullscreen output into a normal window (frame, taskbar button, not topmost, can take
/// focus so Esc works), centred on the monitor it covered, with a client area of `client` clamped to
/// that monitor. It does not take the focus itself. Used when the source is lost for too long (T-56).
pub fn drop_to_windowed(hwnd: HWND, client: (i32, i32)) {
    NO_ACTIVATE.store(false, Ordering::Relaxed);
    // A menu open now must not restore the non-activating mode when it closes.
    MENU_SAVED_NO_ACTIVATE.store(false, Ordering::Relaxed);
    unsafe {
        let mon = monitor_rect(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST)).unwrap_or(RECT {
            left: 0,
            top: 0,
            right: 1280,
            bottom: 720,
        });
        let style = WS_OVERLAPPEDWINDOW;
        let mut r = RECT {
            left: 0,
            top: 0,
            right: client.0.clamp(160, ((mon.right - mon.left) * 3 / 4).max(160)),
            bottom: client.1.clamp(120, ((mon.bottom - mon.top) * 3 / 4).max(120)),
        };
        let _ = AdjustWindowRectEx(&mut r, style, false, WINDOW_EX_STYLE(0));
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        // Hidden while the styles change so the taskbar picks up the removed tool-window style.
        let _ = ShowWindow(hwnd, SW_HIDE);
        SetWindowLongPtrW(hwnd, GWL_STYLE, style.0 as isize);
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, 0);
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_NOTOPMOST),
            mon.left + ((mon.right - mon.left) - w) / 2,
            mon.top + ((mon.bottom - mon.top) - h) / 2,
            w,
            h,
            SWP_FRAMECHANGED | SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
    }
}

/// Processes pending messages. Returns false once the window asked to quit.
pub fn pump() -> bool {
    unsafe {
        let fg = GetForegroundWindow().0 as isize;
        if fg != 0 && fg != OWN.load(Ordering::Relaxed) {
            LAST_FOREGROUND.store(fg, Ordering::Relaxed);
        }
        let mut msg = MSG::default();
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            if msg.message == WM_QUIT {
                return false;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    true
}

pub fn client_size(hwnd: HWND) -> (u32, u32) {
    let mut r = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut r);
    }
    ((r.right - r.left).max(0) as u32, (r.bottom - r.top).max(0) as u32)
}

pub fn set_title(hwnd: HWND, title: &str) {
    unsafe {
        let _ = SetWindowTextW(hwnd, &HSTRING::from(title));
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if let Some(r) = menu_wndproc(hwnd, msg, wparam, lparam) {
        return r;
    }
    match msg {
        WM_MOUSEACTIVATE if NO_ACTIVATE.load(Ordering::Relaxed) => LRESULT(MA_NOACTIVATE as isize),
        WM_ACTIVATE => {
            let target = hand_back_target(
                NO_ACTIVATE.load(Ordering::Relaxed),
                (wparam.0 & 0xFFFF) as u32 != WA_INACTIVE,
                hwnd.0 as isize,
                lparam.0,
                LAST_FOREGROUND.load(Ordering::Relaxed),
                SOURCE.load(Ordering::Relaxed),
            );
            match target {
                // Answered without DefWindowProc, which would move the keyboard focus to the output.
                Some(h) => {
                    let _ = SetForegroundWindow(HWND(h as *mut std::ffi::c_void));
                    HANDBACKS.fetch_add(1, Ordering::Relaxed);
                    LRESULT(0)
                }
                None => DefWindowProcW(hwnd, msg, wparam, lparam),
            }
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_KEYDOWN if wparam.0 == 0x1B => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_HOTKEY => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

#[cfg(test)]
mod tests {
    use super::hand_back_target;

    const OWN: isize = 0x10;
    const FROM: isize = 0x20;
    const LAST: isize = 0x30;
    const SOURCE: isize = 0x40;

    #[test]
    fn an_activating_output_keeps_its_activation() {
        assert_eq!(hand_back_target(false, true, OWN, FROM, LAST, SOURCE), None);
    }

    #[test]
    fn a_deactivation_is_left_alone() {
        assert_eq!(hand_back_target(true, false, OWN, FROM, LAST, SOURCE), None);
    }

    #[test]
    fn the_window_the_activation_came_from_comes_first() {
        assert_eq!(hand_back_target(true, true, OWN, FROM, LAST, SOURCE), Some(FROM));
    }

    #[test]
    fn across_threads_the_last_foreground_then_the_source_is_used() {
        // WM_ACTIVATE gives 0 when the previous window belongs to another thread.
        assert_eq!(hand_back_target(true, true, OWN, 0, LAST, SOURCE), Some(LAST));
        assert_eq!(hand_back_target(true, true, OWN, 0, 0, SOURCE), Some(SOURCE));
    }

    #[test]
    fn the_output_itself_and_unknown_windows_are_never_targets() {
        assert_eq!(hand_back_target(true, true, OWN, OWN, OWN, SOURCE), Some(SOURCE));
        assert_eq!(hand_back_target(true, true, OWN, 0, 0, 0), None);
        assert_eq!(hand_back_target(true, true, OWN, OWN, 0, OWN), None);
    }
}
