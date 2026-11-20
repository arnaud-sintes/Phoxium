//! The output window (raw Win32).
//!
//! Two modes, both non-activating except while the settings menu is open (OUT-2, OUT-3):
//! - windowed: a resizable framed window; it can be moved and resized by its title bar and frame, a click does
//!   not activate it;
//! - fullscreen: borderless, topmost, covering one monitor.
//!
//! Quitting: Ctrl+Alt+Q (always registered) or the close button. Esc does not quit (it belongs to the captured
//! program, and to the menu while it is open). Both quit paths only hide the window and post `WM_QUIT`
//! ([`request_quit`]); the window is destroyed by `Drop for Output`, after the application has run its shutdown
//! and dropped the renderer, so the Vulkan surface never outlives its window (contract of
//! `gpu::Context::with_window`).
//!
//! F11 switches between them at run time ([`apply_placement`], decided by `modes::ModeState`).
//!
//! Moving or resizing the framed window: Windows activates it for its modal move loop whatever the styles; the
//! hand-back then waits for `WM_EXITSIZEMOVE` ([`on_activate`]), so the drag works and the foreground returns
//! when the button is released.
//!
//! Activation: [`sync_activation`] is the only code that sets `WS_EX_NOACTIVATE` and the hand-back flag, from
//! the mode (every mode is non-activating) and the menu state ([`effective_no_activate`]: the open menu always
//! makes the output activating, closing it restores the mode's behaviour).
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
//!
//! Settings menu (T-52a, D-7): while it is open ([`set_menu_open`]) the output may be activated (no
//! `WS_EX_NOACTIVATE`, no hand-back: the menu took the foreground on purpose), its input messages are queued
//! for the UI ([`take_ui_messages`], `crate::ui::input::routed`), the mouse is captured during a button press
//! and the cursor shape is the one ImGui asks for ([`set_menu_cursor`]).

use std::cell::RefCell;
use std::sync::{
    atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering},
    Mutex,
};

use windows::{
    core::{w, Result, HRESULT, HSTRING, PCWSTR},
    Win32::{
        Foundation::{E_INVALIDARG, HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::Gdi::{
            EnumDisplayMonitors, GetMonitorInfoW, MonitorFromWindow, HDC, HMONITOR, MONITORINFO,
            MONITOR_DEFAULTTONEAREST,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Input::KeyboardAndMouse::{
                GetCapture, RegisterHotKey, ReleaseCapture, SetCapture, TrackMouseEvent, UnregisterHotKey,
                HOT_KEY_MODIFIERS, MOD_NOREPEAT, TME_LEAVE, TRACKMOUSEEVENT,
            },
            WindowsAndMessaging::*,
        },
    },
};

use crate::{
    hotkeys::{self, Action, Bindings, Chord, Registrar, Registration},
    modes::{self, IRect, Placement},
    ui::input as ui_input,
};

/// Hotkey actions received since the last [`take_actions`], in order.
static ACTIONS: Mutex<Vec<Action>> = Mutex::new(Vec::new());
/// Set by `WM_DISPLAYCHANGE` (monitor layout or resolution changed), cleared by [`take_display_changed`].
static DISPLAY_CHANGED: AtomicBool = AtomicBool::new(false);
/// The host cursor is hidden over the client area (menu closed, output not click-through, T-57 and CAP-6).
static HIDE_CURSOR: AtomicBool = AtomicBool::new(false);
/// The output currently is the borderless fullscreen one (it ignores `WM_DPICHANGED`).
static FULLSCREEN: AtomicBool = AtomicBool::new(false);
/// The window mode wants a non-activating output (every mode, set by [`create`] and [`apply_placement`]).
/// See [`sync_activation`].
static MODE_NO_ACTIVATE: AtomicBool = AtomicBool::new(false);
/// Set by `Drop for Output` while it destroys the window: `WM_DESTROY` then posts no `WM_QUIT` (a pending
/// `WM_QUIT` would close the error message box `main` may show next).
static DESTROYING: AtomicBool = AtomicBool::new(false);
/// The settings menu is open (T-52a): input goes to the UI and the output may be active.
static MENU_OPEN: AtomicBool = AtomicBool::new(false);
/// System cursor (`IDC_*` number) shown over the client area while the menu is open; 0 hides it.
static MENU_CURSOR: AtomicU32 = AtomicU32::new(32512);
/// `WM_MOUSELEAVE` is requested (`TrackMouseEvent`) for the current stay of the mouse over the window.
static MOUSE_TRACKED: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// Input messages of the output window for the UI (message, `WPARAM`, `LPARAM`), queued by the window
    /// procedure while the menu is open (and DPI changes always), taken by the app every frame.
    static UI_MESSAGES: RefCell<Vec<(u32, usize, isize)>> = const { RefCell::new(Vec::new()) };
}

/// Whether the output is non-activating (`WS_EX_NOACTIVATE` set, the T-07 hand-back on): the window mode wants
/// it and the settings menu is closed. The open menu always wins: it took the foreground on purpose and the
/// hand-back would undo it, whatever the mode (F11 while the menu is open keeps it usable). Pure.
pub fn effective_no_activate(mode_no_activate: bool, menu_open: bool) -> bool {
    mode_no_activate && !menu_open
}

/// The one place that sets the activation behaviour of the output: `WS_EX_NOACTIVATE` and the flag the
/// hand-back of [`wndproc`] reads, from [`effective_no_activate`] of the mode ([`apply_placement`], [`create`])
/// and the menu ([`set_menu_open`]). Only that bit of the extended style changes.
fn sync_activation(hwnd: HWND) {
    let on = effective_no_activate(
        MODE_NO_ACTIVATE.load(Ordering::Relaxed),
        MENU_OPEN.load(Ordering::Relaxed),
    );
    NO_ACTIVATE.store(on, Ordering::Relaxed);
    if hwnd.is_invalid() {
        return;
    }
    // SAFETY: our own window; only the WS_EX_NOACTIVATE bit of the extended style changes.
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let bit = WS_EX_NOACTIVATE.0 as isize;
        let want = if on { ex | bit } else { ex & !bit };
        if want != ex {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, want);
        }
    }
}

/// Opens or closes the menu mode of the output window `hwnd` (T-52a, called by `crate::ui::menu::Menu`).
/// Open: the output becomes activating and the T-07 hand-back stops ([`sync_activation`]), input is queued
/// for the UI. Close: queueing stops, the activation behaviour of the current window mode comes back (a mode
/// changed with F11 while the menu was open included). The caller moves the foreground afterwards.
pub fn set_menu_open(hwnd: HWND, open: bool) {
    if MENU_OPEN.load(Ordering::Relaxed) == open {
        return;
    }
    MENU_OPEN.store(open, Ordering::Relaxed);
    MOUSE_TRACKED.store(false, Ordering::Relaxed);
    if !open {
        UI_MESSAGES.with(|q| q.borrow_mut().clear());
    }
    sync_activation(hwnd);
    if !open {
        // SAFETY: our own window.
        unsafe {
            if GetCapture() == hwnd {
                let _ = ReleaseCapture();
            }
        }
    }
}

/// Whether the settings menu is open (the window procedure routes the input to the UI).
pub fn menu_open() -> bool {
    MENU_OPEN.load(Ordering::Relaxed)
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

/// The effective non-activating state ([`sync_activation`]), read by the window procedure.
static NO_ACTIVATE: AtomicBool = AtomicBool::new(false);
/// The output window, 0 before it exists.
static OWN: AtomicIsize = AtomicIsize::new(0);
/// The source window (`Options::follow`), 0 if none.
static SOURCE: AtomicIsize = AtomicIsize::new(0);
/// The last foreground window that was not the output, sampled at every [`pump`].
static LAST_FOREGROUND: AtomicIsize = AtomicIsize::new(0);
/// Between `WM_ENTERSIZEMOVE` and `WM_EXITSIZEMOVE`: the user moves or resizes the framed output.
static IN_SIZE_MOVE: AtomicBool = AtomicBool::new(false);
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

/// What the window procedure does with `WM_ACTIVATE`.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum OnActivate {
    /// `DefWindowProc`: the output may be active (activating mode, open menu), a deactivation, or no target.
    Default,
    /// Hand the foreground to this window at once, without `DefWindowProc` (the keyboard focus stays away).
    HandBack(isize),
    /// The user is moving or resizing the non-activating output by its title bar or frame (Windows activates the
    /// window for its modal loop): no `DefWindowProc`, the hand-back happens at `WM_EXITSIZEMOVE`; moving the
    /// foreground in the middle of the loop would end the drag.
    AfterSizeMove,
}

/// The `WM_ACTIVATE` decision ([`hand_back_target`] for the target), `in_size_move` between `WM_ENTERSIZEMOVE`
/// and `WM_EXITSIZEMOVE`. Pure.
pub fn on_activate(
    no_activate: bool,
    activated: bool,
    in_size_move: bool,
    own: isize,
    from: isize,
    last: isize,
    source: isize,
) -> OnActivate {
    if no_activate && activated && in_size_move {
        return OnActivate::AfterSizeMove;
    }
    hand_back_target(no_activate, activated, own, from, last, source).map_or(OnActivate::Default, OnActivate::HandBack)
}

/// Hit-test codes of the title bar and the frame (`WM_NCHITTEST`).
const HT_CAPTION: u32 = 2;
const HT_LEFT: u32 = 10;
const HT_RIGHT: u32 = 11;
const HT_TOP: u32 = 12;
const HT_TOPLEFT: u32 = 13;
const HT_TOPRIGHT: u32 = 14;
const HT_BOTTOM: u32 = 15;
const HT_BOTTOMLEFT: u32 = 16;
const HT_BOTTOMRIGHT: u32 = 17;

/// Whether a left press on this hit-test code starts a move (title bar) or a resize (frame edge or corner) that
/// the output runs itself ([`drag_rect`]) instead of Windows' modal loop, which activates the window. Pure.
pub fn self_drag_hit(hit: u32) -> bool {
    hit == HT_CAPTION || (HT_LEFT..=HT_BOTTOMRIGHT).contains(&hit)
}

/// The outer rectangle during a drag started on `hit` with the window at `start`, the pointer moved by
/// (`dx`, `dy`): the title bar moves the window, an edge or corner moves only its sides, the size never going
/// below `min` (the opposite side stays put). Pure.
pub fn drag_rect(hit: u32, start: IRect, dx: i32, dy: i32, min: (i32, i32)) -> IRect {
    if hit == HT_CAPTION {
        return IRect::new(start.left + dx, start.top + dy, start.right + dx, start.bottom + dy);
    }
    let mut r = start;
    if matches!(hit, HT_LEFT | HT_TOPLEFT | HT_BOTTOMLEFT) {
        r.left = (start.left + dx).min(start.right - min.0);
    }
    if matches!(hit, HT_RIGHT | HT_TOPRIGHT | HT_BOTTOMRIGHT) {
        r.right = (start.right + dx).max(start.left + min.0);
    }
    if matches!(hit, HT_TOP | HT_TOPLEFT | HT_TOPRIGHT) {
        r.top = (start.top + dy).min(start.bottom - min.1);
    }
    if matches!(hit, HT_BOTTOM | HT_BOTTOMLEFT | HT_BOTTOMRIGHT) {
        r.bottom = (start.bottom + dy).max(start.top + min.1);
    }
    r
}

/// A move or resize of the non-activating framed output in progress: the hit-test code, the pointer and the
/// outer rectangle at the press.
#[derive(Clone, Copy)]
struct Drag {
    hit: u32,
    from: (i32, i32),
    start: IRect,
}

thread_local! {
    static DRAG: std::cell::Cell<Option<Drag>> = const { std::cell::Cell::new(None) };
}

/// The source window changed (an attach in the picker, a reattach, a detach: 0), the last fallback of the T-07
/// hand-back (A23).
pub fn set_source(source: HWND) {
    SOURCE.store(source.0 as isize, Ordering::Relaxed);
}

/// Activations handed back since the last call (for a console line).
pub fn take_handbacks() -> u32 {
    HANDBACKS.swap(0, Ordering::Relaxed)
}

pub struct Options {
    pub fullscreen: bool,
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

/// Applies a mode change decided by `modes::ModeState`: borderless fullscreen over `cover`, or the framed
/// resizable window at `rect`. Both modes reached this way are non-activating, except while the settings menu
/// is open (the output is then the foreground window and stays so; [`sync_activation`] decides). The window is
/// hidden while the styles change so that the taskbar picks up the tool-window style, except when it holds the
/// foreground (hiding would pass it on). Never activates.
pub fn apply_placement(hwnd: HWND, placement: &Placement) {
    MODE_NO_ACTIVATE.store(true, Ordering::Relaxed);
    // SAFETY: our own live window.
    unsafe {
        if GetForegroundWindow() != hwnd {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
        match *placement {
            Placement::Fullscreen { cover } => {
                FULLSCREEN.store(true, Ordering::Relaxed);
                SetWindowLongPtrW(hwnd, GWL_STYLE, (WS_POPUP | WS_VISIBLE).0 as isize);
                SetWindowLongPtrW(hwnd, GWL_EXSTYLE, (WS_EX_TOPMOST | WS_EX_TOOLWINDOW).0 as isize);
                sync_activation(hwnd);
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
                SetWindowLongPtrW(hwnd, GWL_EXSTYLE, 0);
                sync_activation(hwnd);
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

/// Refresh rate (Hz) of the monitor that holds most of `hwnd` (`EnumDisplaySettingsW`, current mode), `None` when
/// unknown or reported as the hardware default (0 or 1). The automatic quality drop compares the GPU frame time
/// with its period (T-48, OUT-6).
pub fn refresh_hz(hwnd: HWND) -> Option<f64> {
    use windows::Win32::Graphics::Gdi::{EnumDisplaySettingsW, DEVMODEW, ENUM_CURRENT_SETTINGS, MONITORINFOEXW};
    unsafe {
        let m = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if !GetMonitorInfoW(m, &mut info as *mut MONITORINFOEXW as *mut MONITORINFO).as_bool() {
            return None;
        }
        let mut mode = DEVMODEW {
            dmSize: std::mem::size_of::<DEVMODEW>() as u16,
            ..Default::default()
        };
        if !EnumDisplaySettingsW(PCWSTR(info.szDevice.as_ptr()), ENUM_CURRENT_SETTINGS, &mut mode).as_bool() {
            return None;
        }
        (mode.dmDisplayFrequency > 1).then_some(f64::from(mode.dmDisplayFrequency))
    }
}

fn monitor_rect(m: HMONITOR) -> Option<RECT> {
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe { GetMonitorInfoW(m, &mut info).as_bool().then_some(info.rcMonitor) }
}

/// The executable's icon (icon group 1, embedded by build.rs) at the system's large or small icon size for the
/// system DPI; a null icon (Windows' default) if it cannot be loaded.
fn app_icon(hinstance: windows::Win32::Foundation::HINSTANCE, small: bool) -> HICON {
    use windows::Win32::UI::HiDpi::{GetDpiForSystem, GetSystemMetricsForDpi};
    let (cx, cy) = if small {
        (SM_CXSMICON, SM_CYSMICON)
    } else {
        (SM_CXICON, SM_CYICON)
    };
    unsafe {
        let dpi = GetDpiForSystem();
        let (w, h) = (GetSystemMetricsForDpi(cx, dpi), GetSystemMetricsForDpi(cy, dpi));
        LoadImageW(
            Some(hinstance),
            PCWSTR(std::ptr::without_provenance(1)),
            IMAGE_ICON,
            w,
            h,
            LR_DEFAULTCOLOR,
        )
        .map(|h| HICON(h.0))
        .unwrap_or_default()
    }
}

pub fn create(opts: &Options) -> Result<Output> {
    // Every mode starts non-activating (OUT-2, OUT-3), the default framed window included: starting, clicking or
    // dragging the output leaves the foreground and the keyboard focus on the captured window. Only the settings
    // menu activates it.
    MODE_NO_ACTIVATE.store(true, Ordering::Relaxed);
    DESTROYING.store(false, Ordering::Relaxed);
    IN_SIZE_MOVE.store(false, Ordering::Relaxed);
    MENU_OPEN.store(false, Ordering::Relaxed);
    // No window yet: only the flag the window procedure reads during the creation.
    sync_activation(HWND::default());
    SOURCE.store(opts.follow.0 as isize, Ordering::Relaxed);
    LAST_FOREGROUND.store(unsafe { GetForegroundWindow() }.0 as isize, Ordering::Relaxed);
    unsafe {
        let hinstance = GetModuleHandleW(None)?;
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            // Title bar, taskbar button and Alt+Tab: the executable's icon (assets/icon, embedded by build.rs).
            hIcon: app_icon(hinstance.into(), false),
            hIconSm: app_icon(hinstance.into(), true),
            lpszClassName: w!("ultimatecrt_output"),
            ..Default::default()
        };
        RegisterClassExW(&class);

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
            // Non-activating like every mode (OUT-2); `sync_activation` below keeps the bit in step with the menu.
            let ex = if NO_ACTIVATE.load(Ordering::Relaxed) {
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
            DESTROYING.store(true, Ordering::Relaxed); // no WM_QUIT: the error message box comes next
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
        sync_activation(hwnd);
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
    /// Destroys the window: the last step of the shutdown, after the renderer (and its Vulkan surface) is gone.
    fn drop(&mut self) {
        DESTROYING.store(true, Ordering::Relaxed);
        // SAFETY: our own window, destroyed on the thread that created it.
        unsafe {
            self.hotkeys.unregister_all(&mut Win32Registrar { hwnd: self.hwnd });
            if IsWindow(Some(self.hwnd)).as_bool() {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
                let _ = DestroyWindow(self.hwnd);
            }
        }
    }
}

/// Hides the output window (the first step of the shutdown, on every exit path): the screen shows what is
/// under it at once, while the saves run and the renderer is released. The window itself stays alive until
/// `Drop for Output`.
pub fn hide(hwnd: HWND) {
    // SAFETY: plain call on our own window; a no-op on a null or destroyed handle.
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
}

/// The quit paths (Ctrl+Alt+Q, the close button): hide the window and leave the message loop. The window is not
/// destroyed here (see the module documentation).
fn request_quit(hwnd: HWND) {
    hide(hwnd);
    // SAFETY: plain call.
    unsafe { PostQuitMessage(0) };
}

/// Whether the loop should idle instead of rendering (R-04): the output is minimised or has an empty client
/// area. Pure.
pub fn should_idle(client: (u32, u32), iconic: bool) -> bool {
    iconic || client.0 == 0 || client.1 == 0
}

/// Whether the output window is minimised.
pub fn is_iconic(hwnd: HWND) -> bool {
    // SAFETY: plain query on a window handle.
    unsafe { IsIconic(hwnd) }.as_bool()
}

/// Sleeps until a message arrives for this thread (a hotkey, a restore, any input) or `timeout` passes, with no
/// CPU use meanwhile (the idle wait of a minimised output, R-04). The caller then runs [`pump`].
pub fn wait_messages(timeout: std::time::Duration) {
    let ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
    // SAFETY: plain call with no handles; MWMO_INPUTAVAILABLE also returns for messages already queued.
    unsafe {
        let _ = MsgWaitForMultipleObjectsEx(None, ms, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
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

/// D-05: a panic cannot unwind out of an `extern "system"` function (the process would abort with no report):
/// it is caught here, recorded for the loop (`crash::take_callback_panic`, which ends the session through the
/// shutdown), and the message gets the default answer.
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY (closure): the same call as this function's; DefWindowProcW below on the arguments we were given.
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        wndproc_body(hwnd, msg, wparam, lparam)
    })) {
        Ok(r) => r,
        Err(payload) => {
            crate::crash::record_callback_panic(&*payload);
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
    }
}

unsafe fn wndproc_body(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if let Some(r) = menu_wndproc(hwnd, msg, wparam, lparam) {
        return r;
    }
    match msg {
        WM_MOUSEACTIVATE if NO_ACTIVATE.load(Ordering::Relaxed) => LRESULT(MA_NOACTIVATE as isize),
        WM_ACTIVATE => {
            match on_activate(
                NO_ACTIVATE.load(Ordering::Relaxed),
                (wparam.0 & 0xFFFF) as u32 != WA_INACTIVE,
                IN_SIZE_MOVE.load(Ordering::Relaxed),
                hwnd.0 as isize,
                lparam.0,
                LAST_FOREGROUND.load(Ordering::Relaxed),
                SOURCE.load(Ordering::Relaxed),
            ) {
                // Answered without DefWindowProc, which would move the keyboard focus to the output.
                OnActivate::HandBack(h) => {
                    let _ = SetForegroundWindow(HWND(h as *mut std::ffi::c_void));
                    HANDBACKS.fetch_add(1, Ordering::Relaxed);
                    LRESULT(0)
                }
                OnActivate::AfterSizeMove => LRESULT(0),
                OnActivate::Default => DefWindowProcW(hwnd, msg, wparam, lparam),
            }
        }
        // Moving or resizing the non-activating framed output: run here, not in Windows' modal loop, which would
        // activate the window and give it the keyboard focus (OUT-2). Aero snap and the keyboard move do not apply.
        WM_NCLBUTTONDOWN
            if NO_ACTIVATE.load(Ordering::Relaxed)
                && !FULLSCREEN.load(Ordering::Relaxed)
                && self_drag_hit(wparam.0 as u32) =>
        {
            let mut p = windows::Win32::Foundation::POINT::default();
            let _ = GetCursorPos(&mut p);
            DRAG.set(Some(Drag {
                hit: wparam.0 as u32,
                from: (p.x, p.y),
                start: outer_rect(hwnd),
            }));
            SetCapture(hwnd);
            LRESULT(0)
        }
        WM_MOUSEMOVE if DRAG.get().is_some() => {
            if let Some(d) = DRAG.get() {
                let mut p = windows::Win32::Foundation::POINT::default();
                let _ = GetCursorPos(&mut p);
                let min = (GetSystemMetrics(SM_CXMINTRACK), GetSystemMetrics(SM_CYMINTRACK));
                let r = drag_rect(d.hit, d.start, p.x - d.from.0, p.y - d.from.1, min);
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    r.left,
                    r.top,
                    r.width(),
                    r.height(),
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            LRESULT(0)
        }
        WM_LBUTTONUP if DRAG.get().is_some() => {
            DRAG.set(None);
            let _ = ReleaseCapture();
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            DRAG.set(None);
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_ENTERSIZEMOVE => {
            IN_SIZE_MOVE.store(true, Ordering::Relaxed);
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_EXITSIZEMOVE => {
            IN_SIZE_MOVE.store(false, Ordering::Relaxed);
            // The drag is over: the foreground the move loop gave the output goes back (no-op if it has none).
            let target = hand_back_target(
                NO_ACTIVATE.load(Ordering::Relaxed),
                GetForegroundWindow() == hwnd,
                hwnd.0 as isize,
                0,
                LAST_FOREGROUND.load(Ordering::Relaxed),
                SOURCE.load(Ordering::Relaxed),
            );
            if let Some(h) = target {
                let _ = SetForegroundWindow(HWND(h as *mut std::ffi::c_void));
                HANDBACKS.fetch_add(1, Ordering::Relaxed);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_ERASEBKGND => LRESULT(1),
        // The close button (and Alt+F4 while the menu has the focus): the same path as Ctrl+Alt+Q.
        WM_CLOSE => {
            request_quit(hwnd);
            LRESULT(0)
        }
        WM_HOTKEY => {
            if let Some(action) = Action::from_hotkey_id(wparam.0 as i32) {
                if action == Action::Quit {
                    // The window disappears at once, also when the loop is busy; the loop then shuts down.
                    request_quit(hwnd);
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
        // Only `Drop for Output` destroys the window; any other destruction still ends the loop.
        WM_DESTROY => {
            if !DESTROYING.load(Ordering::Relaxed) {
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        drag_rect, effective_no_activate, hand_back_target, on_activate, self_drag_hit, should_idle, OnActivate,
    };
    use crate::modes::IRect;

    /// R-04: the loop idles while the output is minimised or has an empty client area.
    #[test]
    fn a_minimised_or_empty_output_idles() {
        assert!(!should_idle((1280, 720), false));
        assert!(should_idle((1280, 720), true), "minimised (some drivers keep the size)");
        assert!(should_idle((0, 0), false));
        assert!(should_idle((0, 720), false));
        assert!(should_idle((1280, 0), false));
        assert!(should_idle((0, 0), true));
    }

    #[test]
    fn the_title_bar_and_the_frame_are_dragged_by_the_output_itself() {
        assert!(self_drag_hit(2)); // HTCAPTION
        assert!((10..=17).all(self_drag_hit)); // HTLEFT .. HTBOTTOMRIGHT
        assert!(!self_drag_hit(1)); // HTCLIENT
        assert!(!self_drag_hit(20)); // HTCLOSE: the caption buttons keep the default handling
        assert!(!self_drag_hit(9)); // HTMAXBUTTON
    }

    #[test]
    fn a_drag_moves_by_the_title_bar_and_resizes_by_the_frame_within_the_minimum() {
        let s = IRect::new(100, 100, 900, 700);
        let min = (136, 39);
        assert_eq!(drag_rect(2, s, 120, 80, min), IRect::new(220, 180, 1020, 780));
        assert_eq!(drag_rect(17, s, 100, 60, min), IRect::new(100, 100, 1000, 760)); // bottom-right
        assert_eq!(drag_rect(13, s, -50, -20, min), IRect::new(50, 80, 900, 700)); // top-left
        assert_eq!(drag_rect(10, s, 30, 999, min), IRect::new(130, 100, 900, 700)); // left: no vertical change
        assert_eq!(drag_rect(15, s, 999, -30, min), IRect::new(100, 100, 900, 670)); // bottom only
                                                                                     // Never below the minimum size; the opposite side stays where it was.
        assert_eq!(drag_rect(11, s, -5000, 0, min), IRect::new(100, 100, 236, 700));
        assert_eq!(drag_rect(12, s, 0, 5000, min), IRect::new(100, 661, 900, 700));
    }

    #[test]
    fn a_drag_of_the_framed_output_defers_the_hand_back_to_the_end_of_the_move() {
        // Activated by Windows' move loop: wait (no DefWindowProc), the hand-back comes at WM_EXITSIZEMOVE.
        assert_eq!(
            on_activate(true, true, true, OWN, FROM, LAST, SOURCE),
            OnActivate::AfterSizeMove
        );
        // Outside a drag: at once, as before.
        assert_eq!(
            on_activate(true, true, false, OWN, FROM, LAST, SOURCE),
            OnActivate::HandBack(FROM)
        );
        // The open menu (activating) and deactivations keep the default handling, drag or not.
        assert_eq!(
            on_activate(false, true, true, OWN, FROM, LAST, SOURCE),
            OnActivate::Default
        );
        assert_eq!(
            on_activate(true, false, true, OWN, FROM, LAST, SOURCE),
            OnActivate::Default
        );
        // No known target: default handling.
        assert_eq!(on_activate(true, true, false, OWN, 0, 0, 0), OnActivate::Default);
        // At the end of the drag (`from` unknown): the last foreground window, else the source.
        assert_eq!(hand_back_target(true, true, OWN, 0, LAST, SOURCE), Some(LAST));
    }

    #[test]
    fn the_open_menu_makes_any_mode_activating_and_closing_it_restores_the_mode() {
        // Every mode (windowed default included): non-activating while the menu is closed.
        assert!(effective_no_activate(true, false));
        // The menu open (before or after an F11): activating, no hand-back.
        assert!(!effective_no_activate(true, true));
        // A drop to a window (T-56) is activating with or without the menu.
        assert!(!effective_no_activate(false, false));
        assert!(!effective_no_activate(false, true));
        // The hand-back follows: none while the menu is open.
        assert_eq!(
            hand_back_target(effective_no_activate(true, true), true, OWN, FROM, LAST, SOURCE),
            None
        );
        assert_eq!(
            hand_back_target(effective_no_activate(true, false), true, OWN, FROM, LAST, SOURCE),
            Some(FROM)
        );
    }

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
