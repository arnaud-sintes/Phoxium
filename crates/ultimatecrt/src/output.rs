//! The output window (raw Win32).
//!
//! Two modes:
//! - windowed: a normal resizable window, Esc closes it (when it is activating);
//! - fullscreen: borderless, topmost, non-activating, covering one monitor, never takes focus.
//!
//! F11 switches between them at run time ([`apply_placement`], decided by `modes::ModeState`); the framed
//! window reached that way is non-activating too, except while the settings menu is open (T-57).
//!
//! The hotkeys (T-54) are `RegisterHotKey` registrations on this window ([`hotkeys`]): Ctrl+Alt+Q always quits,
//! since a non-activating fullscreen window cannot receive keyboard input; the others arrive as `WM_HOTKEY`,
//! become [`Action`]s in a queue that the application loop drains with [`take_actions`].
//!
//! Hand-back (T-07): `WS_EX_NOACTIVATE`, `SW_SHOWNOACTIVATE`, `MA_NOACTIVATE`, `SWP_NOACTIVATE` and a
//! disabled window do not stop Windows from activating the window once, about 25 s after the
//! process starts (initiator outside the program; it happens with no Vulkan and no capture, see
//! QUESTIONS OUT-2). A non-activating output therefore hands the foreground back at once when it is
//! activated: to the window it came from, else the last other foreground window sampled by [`pump`],
//! else the source window. `WM_ACTIVATE` is answered without `DefWindowProc`, so the keyboard focus
//! never moves to the output; what remains is a foreground blip of about 0.1 s.

use std::sync::{
    atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering},
    Mutex,
};

use windows::{
    core::{w, Result, HRESULT, HSTRING},
    Win32::{
        Foundation::{E_INVALIDARG, HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::Gdi::{
            EnumDisplayMonitors, GetMonitorInfoW, MonitorFromWindow, HDC, HMONITOR, MONITORINFO,
            MONITOR_DEFAULTTONEAREST,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_NOREPEAT},
            WindowsAndMessaging::*,
        },
    },
};

use crate::{
    hotkeys::{self, Action, Bindings, Chord, Registrar, Registration},
    modes::{self, IRect, Placement},
};

/// Hotkey actions received since the last [`take_actions`], in order.
static ACTIONS: Mutex<Vec<Action>> = Mutex::new(Vec::new());
/// Set by `WM_DISPLAYCHANGE` (monitor layout or resolution changed), cleared by [`take_display_changed`].
static DISPLAY_CHANGED: AtomicBool = AtomicBool::new(false);
/// The host cursor is hidden over the client area (menu closed, output not click-through, T-57 and CAP-6).
static HIDE_CURSOR: AtomicBool = AtomicBool::new(false);
/// The output currently is the borderless fullscreen one (it ignores `WM_DPICHANGED`).
static FULLSCREEN: AtomicBool = AtomicBool::new(false);
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
    /// The hotkey chords wanted (T-54), from `Bindings::from_settings`.
    pub hotkeys: Bindings,
    /// Outer rectangle of the windowed output remembered in the settings (T-57), used when it is still on a monitor.
    pub saved_rect: Option<IRect>,
}

pub struct Output {
    pub hwnd: HWND,
    pub hinstance: isize,
    /// How the hotkeys were registered: the chord in force per action and the problems (the UI shows them).
    pub hotkeys: Registration,
}

/// `RegisterHotKey` on the output window.
struct Win32Registrar {
    hwnd: HWND,
}

impl Registrar for Win32Registrar {
    fn register(&mut self, id: i32, chord: &Chord) -> std::result::Result<(), u32> {
        // SAFETY: our own live window; the hotkeys are unregistered in `Drop for Output`.
        unsafe {
            RegisterHotKey(
                Some(self.hwnd),
                id,
                HOT_KEY_MODIFIERS(chord.mods.bits()) | MOD_NOREPEAT,
                u32::from(chord.vk),
            )
        }
        .map_err(|e| hotkeys::win32_code(e.code().0))
    }

    fn unregister(&mut self, id: i32) {
        // SAFETY: as above.
        let _ = unsafe { UnregisterHotKey(Some(self.hwnd), id) };
    }
}

fn irect(r: RECT) -> IRect {
    IRect::new(r.left, r.top, r.right, r.bottom)
}

/// The monitors as rectangles in virtual-screen pixels.
pub fn monitor_irects() -> Vec<IRect> {
    monitor_rects().into_iter().map(irect).collect()
}

/// The monitor nearest to the window.
pub fn monitor_of(hwnd: HWND) -> IRect {
    let fallback = IRect::new(0, 0, 1280, 720);
    // SAFETY: a plain query on a window handle.
    unsafe { monitor_rect(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST)) }.map_or(fallback, irect)
}

/// The outer rectangle of the window (frame included).
pub fn outer_rect(hwnd: HWND) -> IRect {
    let mut r = RECT::default();
    // SAFETY: a plain query on a window handle.
    let _ = unsafe { GetWindowRect(hwnd, &mut r) };
    irect(r)
}

/// The outer size of a framed window with this client size (the frame of `WS_OVERLAPPEDWINDOW`).
pub fn default_outer_size(client: (i32, i32)) -> (i32, i32) {
    let mut r = RECT {
        left: 0,
        top: 0,
        right: client.0,
        bottom: client.1,
    };
    // SAFETY: a pure computation on a local rectangle.
    let _ = unsafe { AdjustWindowRectEx(&mut r, WS_OVERLAPPEDWINDOW, false, WINDOW_EX_STYLE(0)) };
    (r.right - r.left, r.bottom - r.top)
}

/// Hotkey actions received since the last call, in order (drained by the application loop).
pub fn take_actions() -> Vec<Action> {
    std::mem::take(&mut *ACTIONS.lock().unwrap_or_else(|e| e.into_inner()))
}

/// True once after the monitor layout or resolution changed (`WM_DISPLAYCHANGE`).
pub fn take_display_changed() -> bool {
    DISPLAY_CHANGED.swap(false, Ordering::Relaxed)
}

/// Hides or shows the host cursor over the client area of the output (the system cursor is not drawn there
/// while hidden; the source's own cursor is not captured, so nothing replaces it). Call with
/// `!ui.wants_input()` every loop: the menu needs the cursor, the closed output does not.
pub fn set_cursor_hidden(hidden: bool) {
    HIDE_CURSOR.store(hidden, Ordering::Relaxed);
}

/// Makes the output non-activating (the default) or activating (while the settings menu is open): the
/// `WS_EX_NOACTIVATE` style and the flag the hand-back of [`wndproc`] reads, kept together.
pub fn set_no_activate(hwnd: HWND, on: bool) {
    NO_ACTIVATE.store(on, Ordering::Relaxed);
    // SAFETY: our own live window.
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        let ex = if on {
            ex | WS_EX_NOACTIVATE.0
        } else {
            ex & !WS_EX_NOACTIVATE.0
        };
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex as isize);
    }
}

/// Applies a mode change decided by `modes::ModeState`: borderless fullscreen over `cover`, or the framed
/// resizable window at `rect`. `no_activate` is false only while the settings menu is open (the output is then
/// the foreground window and stays so). The window is hidden while the styles change so that the taskbar picks
/// up the tool-window style, except when it holds the foreground (hiding would pass it on). Never activates.
pub fn apply_placement(hwnd: HWND, placement: &Placement, no_activate: bool) {
    NO_ACTIVATE.store(no_activate, Ordering::Relaxed);
    let na = if no_activate {
        WS_EX_NOACTIVATE
    } else {
        WINDOW_EX_STYLE(0)
    };
    // SAFETY: our own live window.
    unsafe {
        if GetForegroundWindow() != hwnd {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
        match *placement {
            Placement::Fullscreen { cover } => {
                FULLSCREEN.store(true, Ordering::Relaxed);
                SetWindowLongPtrW(hwnd, GWL_STYLE, (WS_POPUP | WS_VISIBLE).0 as isize);
                SetWindowLongPtrW(hwnd, GWL_EXSTYLE, (WS_EX_TOPMOST | WS_EX_TOOLWINDOW | na).0 as isize);
                let _ = SetWindowPos(
                    hwnd,
                    Some(HWND_TOPMOST),
                    cover.left,
                    cover.top,
                    cover.width(),
                    cover.height(),
                    SWP_FRAMECHANGED | SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
            }
            Placement::Windowed { rect } => {
                FULLSCREEN.store(false, Ordering::Relaxed);
                SetWindowLongPtrW(hwnd, GWL_STYLE, (WS_OVERLAPPEDWINDOW | WS_VISIBLE).0 as isize);
                SetWindowLongPtrW(hwnd, GWL_EXSTYLE, na.0 as isize);
                let _ = SetWindowPos(
                    hwnd,
                    Some(HWND_NOTOPMOST),
                    rect.left,
                    rect.top,
                    rect.width(),
                    rect.height(),
                    SWP_FRAMECHANGED | SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
            }
        }
    }
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
            // T-57: the rectangle of the last windowed run, when it is still on a monitor.
            let (x, y, w, h) = match opts.saved_rect.filter(|s| modes::usable_rect(s, &monitor_irects())) {
                Some(s) => (s.left, s.top, s.width(), s.height()),
                None => (CW_USEDEFAULT, CW_USEDEFAULT, r.right - r.left, r.bottom - r.top),
            };
            CreateWindowExW(
                ex,
                w!("ultimatecrt_output"),
                &HSTRING::from("ultimatecrt"),
                style,
                x,
                y,
                w,
                h,
                None,
                None,
                Some(hinstance.into()),
                None,
            )?
        };

        // T-54: every hotkey through one plan (wanted chord, then the documented fallbacks); each failure is
        // reported with its chord and Win32 error code, and kept in `Output::hotkeys` for the UI.
        let mut registrar = Win32Registrar { hwnd };
        let registration = hotkeys::register_all(&mut registrar, &hotkeys::plan_registration(&opts.hotkeys));
        for problem in registration.problems() {
            eprintln!("warning: {problem}");
        }
        if registration.bound(Action::Quit).is_none() && opts.fullscreen {
            // Without the hotkey a fullscreen output that cannot take focus has no keyboard exit.
            registration.unregister_all(&mut registrar);
            let _ = DestroyWindow(hwnd);
            let code = registration
                .outcomes
                .iter()
                .find(|o| o.action == Action::Quit)
                .and_then(|o| o.failures.first().map(|f| f.1))
                .unwrap_or(hotkeys::ERROR_HOTKEY_ALREADY_REGISTERED);
            return Err(windows::core::Error::new(
                HRESULT::from_win32(code),
                "cannot register the Ctrl+Alt+Q quit hotkey (another program owns it); refusing to go fullscreen",
            ));
        }
        FULLSCREEN.store(opts.fullscreen, Ordering::Relaxed);
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
            hotkeys: registration,
        })
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        unsafe {
            self.hotkeys.unregister_all(&mut Win32Registrar { hwnd: self.hwnd });
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
    FULLSCREEN.store(false, Ordering::Relaxed);
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
            if let Some(action) = Action::from_hotkey_id(wparam.0 as i32) {
                if action == Action::Quit {
                    // Immediate, as before T-54: the quit key works even if the loop is busy.
                    let _ = DestroyWindow(hwnd);
                }
                ACTIONS.lock().unwrap_or_else(|e| e.into_inner()).push(action);
            }
            LRESULT(0)
        }
        WM_DISPLAYCHANGE => {
            DISPLAY_CHANGED.store(true, Ordering::Relaxed);
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        // The framed window follows the suggested rectangle when it moves to a monitor of another DPI; the
        // fullscreen one keeps the rectangle `apply_placement` gave it.
        WM_DPICHANGED if !FULLSCREEN.load(Ordering::Relaxed) => {
            let r = &*(lparam.0 as *const RECT);
            let _ = SetWindowPos(
                hwnd,
                None,
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            LRESULT(0)
        }
        WM_SETCURSOR if HIDE_CURSOR.load(Ordering::Relaxed) && (lparam.0 & 0xFFFF) as u32 == HTCLIENT => {
            SetCursor(None);
            LRESULT(1)
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
