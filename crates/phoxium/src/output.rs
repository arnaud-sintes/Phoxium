//! The output window (raw Win32).
//!
//! Two modes, both non-activating except while the settings menu is open (OUT-2, OUT-3):
//! - windowed: a resizable framed window; it can be moved and resized by its title bar and frame, a click does
//!   not activate it;
//! - fullscreen: borderless, topmost, covering one monitor.
//!
//! Quitting: the Quit hotkey (Ctrl+Alt+End by default, always registered) or the close button. Esc does not quit (it belongs to the captured
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
//! The hotkeys (T-54) are `RegisterHotKey` registrations on this window ([`hotkeys`]): the Quit hotkey always quits,
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
            EnumDisplayMonitors, FillRect, GetMonitorInfoW, GetStockObject, MonitorFromWindow, BLACK_BRUSH, HBRUSH,
            HDC, HMONITOR, MONITORINFO, MONITOR_DEFAULTTONEAREST,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Input::KeyboardAndMouse::{
                GetAsyncKeyState, GetCapture, RegisterHotKey, ReleaseCapture, SetCapture, TrackMouseEvent,
                UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_NOREPEAT, TME_LEAVE, TRACKMOUSEEVENT,
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

/// Hotkey actions received since the last [`take_actions`], in order, with the chord pressed.
static ACTIONS: Mutex<Vec<(Action, Chord)>> = Mutex::new(Vec::new());
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
/// Click-through is asked (`[global.output] click_through`); see [`behaviour`].
static CLICK_THROUGH: AtomicBool = AtomicBool::new(false);
/// System cursor (`IDC_*` number) shown over the client area while the menu is open; 0 hides it.
static MENU_CURSOR: AtomicU32 = AtomicU32::new(32512);
/// `WM_MOUSELEAVE` is requested (`TrackMouseEvent`) for the current stay of the mouse over the window.
static MOUSE_TRACKED: AtomicBool = AtomicBool::new(false);
/// The output window was shown ([`reveal`]): [`create`] leaves it hidden until the first frame is presented, so
/// the desktop never shows its empty (white) client area. Until then `WM_ERASEBKGND` fills it black.
static REVEALED: AtomicBool = AtomicBool::new(false);
/// `WM_ERASEBKGND` fills the client area black: from [`create`] up to the first erase after [`reveal`].
static ERASE_BLACK: AtomicBool = AtomicBool::new(false);

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

/// How the output window treats activation and the mouse: the extended style bits [`sync_activation`] sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Behaviour {
    /// `WS_EX_NOACTIVATE` and the T-07 hand-back ([`effective_no_activate`]).
    pub no_activate: bool,
    /// `WS_EX_TRANSPARENT | WS_EX_LAYERED` (full alpha): the mouse goes through the output to the window under
    /// it (the captured game).
    pub click_through: bool,
}

/// The behaviour of the output from the window mode (`mode_no_activate`, every mode reached by
/// [`apply_placement`] is non-activating), the click-through setting (`[global.output] click_through`, its
/// hotkey, the menu row) and the menu state. The open menu (settings or window picker) always takes the mouse and
/// the activation back, whatever the mode and the setting: the user can never be locked out of the menu, and
/// closing it returns to the setting. Click-through does not depend on the mode (fullscreen and framed alike;
/// a click-through framed window cannot be moved by the mouse until the menu is open or the setting is off).
/// Pure.
pub fn behaviour(mode_no_activate: bool, click_through: bool, menu_open: bool) -> Behaviour {
    Behaviour {
        no_activate: effective_no_activate(mode_no_activate, menu_open),
        click_through: click_through && !menu_open,
    }
}

/// The extended style of a mode before [`styled`] adds the activation and mouse bits: `WS_EX_APPWINDOW` in every
/// mode, plus `WS_EX_TOPMOST` in fullscreen; never `WS_EX_TOOLWINDOW`. The output is the app's only window and
/// must keep a taskbar button (and an Alt+Tab entry) in both modes: a `WS_EX_NOACTIVATE` window gets none unless
/// `WS_EX_APPWINDOW` is set, and a tool window gets none at all (owner report 2026-11-21, QUESTIONS OUT-2).
/// Activating it from the taskbar or Alt+Tab is handed back like any other activation (T-07). Pure.
pub fn base_ex_style(fullscreen: bool) -> isize {
    let topmost = if fullscreen { WS_EX_TOPMOST.0 } else { 0 };
    (WS_EX_APPWINDOW.0 | topmost) as isize
}

/// The extended style `ex` with the bits of `b` applied (`WS_EX_NOACTIVATE`, `WS_EX_TRANSPARENT`,
/// `WS_EX_LAYERED`); every other bit is kept. Pure.
pub fn styled(ex: isize, b: Behaviour) -> isize {
    let set = |ex: isize, bit: u32, on: bool| {
        if on {
            ex | bit as isize
        } else {
            ex & !(bit as isize)
        }
    };
    let ex = set(ex, WS_EX_NOACTIVATE.0, b.no_activate);
    let ex = set(ex, WS_EX_TRANSPARENT.0, b.click_through);
    set(ex, WS_EX_LAYERED.0, b.click_through)
}

/// The one place that sets the activation and mouse behaviour of the output: `WS_EX_NOACTIVATE`,
/// `WS_EX_TRANSPARENT | WS_EX_LAYERED` and the flags the window procedure reads, from [`behaviour`] of the mode
/// ([`apply_placement`], [`create`]), the click-through setting ([`set_click_through`]) and the menu
/// ([`set_menu_open`]). Only those bits of the extended style change. A layered window gets full opacity at once
/// (a layered window without its attributes is not drawn).
fn sync_activation(hwnd: HWND) {
    let b = behaviour(
        MODE_NO_ACTIVATE.load(Ordering::Relaxed),
        CLICK_THROUGH.load(Ordering::Relaxed),
        MENU_OPEN.load(Ordering::Relaxed),
    );
    NO_ACTIVATE.store(b.no_activate, Ordering::Relaxed);
    if hwnd.is_invalid() {
        return;
    }
    // SAFETY: our own window; only the three bits above of the extended style change.
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let want = styled(ex, b);
        if want != ex {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, want);
        }
        if b.click_through {
            // Opaque: only the hit-testing changes, never the picture.
            let _ = SetLayeredWindowAttributes(hwnd, windows::Win32::Foundation::COLORREF(0), 255, LWA_ALPHA);
        }
    }
}

/// Switches click-through on or off (`[global.output] click_through`, its hotkey, the menu row): applied at once
/// through [`sync_activation`] (while the menu is open it takes effect when the menu closes). A null `hwnd`
/// (before the window exists) only records the setting.
pub fn set_click_through(hwnd: HWND, on: bool) {
    CLICK_THROUGH.store(on, Ordering::Relaxed);
    sync_activation(hwnd);
}

/// Whether click-through is asked (the setting; the open menu suspends it, [`behaviour`]).
pub fn click_through() -> bool {
    CLICK_THROUGH.load(Ordering::Relaxed)
}

/// The keep-awake state last handed to Windows (`[global.output] keep_awake`).
static KEEP_AWAKE: AtomicBool = AtomicBool::new(false);

/// The execution state that keeps the PC awake (`on`) or releases the request: `ES_CONTINUOUS` with the system
/// and display required flags, or `ES_CONTINUOUS` alone (the flags cleared).
pub fn keep_awake_flags(on: bool) -> u32 {
    use windows::Win32::System::Power::{ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED};
    if on {
        (ES_CONTINUOUS | ES_DISPLAY_REQUIRED | ES_SYSTEM_REQUIRED).0
    } else {
        ES_CONTINUOUS.0
    }
}

/// Keeps the PC and its display awake while `on` (`SetThreadExecutionState` of the calling thread, which is the
/// output loop's; Windows drops the request when the thread ends). Cheap to call every frame: Windows is only asked
/// when the state changes.
pub fn set_keep_awake(on: bool) {
    use windows::Win32::System::Power::{SetThreadExecutionState, EXECUTION_STATE};
    if KEEP_AWAKE.swap(on, Ordering::Relaxed) == on {
        return;
    }
    // SAFETY: a plain call with a combination of the documented flags.
    unsafe {
        SetThreadExecutionState(EXECUTION_STATE(keep_awake_flags(on)));
    }
    println!("output: keep awake {}", if on { "on" } else { "off" });
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
        // The menu's input goes; a DPI change queued in the same pump stays (the UI scale must follow it).
        UI_MESSAGES.with(|q| q.borrow_mut().retain(|m| m.0 == ui_input::WM_DPICHANGED));
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

/// A move by the title bar ([`drag_rect`]) carried the framed output onto a monitor of another DPI and Windows
/// suggests `suggested` (`WM_DPICHANGED`): the drag goes on from its press position with the suggested size, so
/// the next pointer move does not put the old size back. A resize by the frame keeps its start (the user drives
/// the size). Pure.
pub fn rebase_drag_on_dpi(hit: u32, start: IRect, suggested: IRect) -> IRect {
    if hit != HT_CAPTION {
        return start;
    }
    IRect::new(
        start.left,
        start.top,
        start.left + suggested.width(),
        start.top + suggested.height(),
    )
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
    // A whole monitor has no window to hand the focus back to.
    let h = source.0 as isize;
    SOURCE.store(if h < 0 { 0 } else { h }, Ordering::Relaxed);
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
    /// `[global.output] click_through`: the mouse goes through the output while the menu is closed.
    pub click_through: bool,
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
    // The handle of a whole monitor as a capture source: that monitor.
    if let Some(m) = crate::sources::monitor_index(hwnd.0 as isize).and_then(|i| monitor_irects().get(i).copied()) {
        return m;
    }
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

/// Hotkey actions received since the last call, in order, each with the chord pressed (`WM_HOTKEY`'s `LPARAM`:
/// the menu takes it as a new chord while it waits for one). Drained by the application loop.
pub fn take_actions() -> Vec<(Action, Chord)> {
    std::mem::take(&mut *ACTIONS.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Whether the key with virtual-key code `vk` is down right now (the physical state, whatever program has the focus
/// and even when a registered hotkey swallowed the press). Used to see the release of a held hotkey, which
/// `RegisterHotKey` does not report.
pub fn key_down(vk: u16) -> bool {
    // SAFETY: a pure query of the keyboard state; the high bit of the result is set while the key is down.
    unsafe { GetAsyncKeyState(i32::from(vk)) < 0 }
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
/// is open (the output is then the foreground window and stays so; [`sync_activation`] decides). Both keep their
/// taskbar button ([`base_ex_style`]). The window is hidden while the styles change so that the taskbar picks
/// them up, except when it holds the foreground (hiding would pass it on). Never activates.
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
                SetWindowLongPtrW(hwnd, GWL_EXSTYLE, base_ex_style(true));
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
                SetWindowLongPtrW(hwnd, GWL_EXSTYLE, base_ex_style(false));
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

/// The monitors, in the order of [`monitor_rects`] (the same index), as handles.
pub fn monitor_handles() -> Vec<HMONITOR> {
    let mut list: Vec<HMONITOR> = Vec::new();
    // SAFETY: plain enumeration; `list` outlives the call.
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(monitor_proc), LPARAM(&mut list as *mut _ as isize));
    }
    list.into_iter().filter(|m| monitor_rect(*m).is_some()).collect()
}

/// Hides the output window from screen captures (`WDA_EXCLUDEFROMCAPTURE`, Windows 10 2004 and later) or shows it
/// again: a captured whole monitor would otherwise contain the output itself (a picture of the picture, over and
/// over). The output's own pixels, the snapshot and the menu are not affected, only what other programs capture
/// of the screen. `false` when the system refused (an older Windows).
pub fn set_capture_excluded(hwnd: HWND, excluded: bool) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE, WDA_NONE};
    // SAFETY: our own live window.
    unsafe { SetWindowDisplayAffinity(hwnd, if excluded { WDA_EXCLUDEFROMCAPTURE } else { WDA_NONE }).is_ok() }
}

pub fn monitor_rects() -> Vec<RECT> {
    let mut list: Vec<HMONITOR> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(monitor_proc), LPARAM(&mut list as *mut _ as isize));
    }
    list.into_iter().filter_map(monitor_rect).collect()
}

/// Every monitor as (whole rectangle, work area without the taskbar), in virtual-screen pixels.
pub fn monitor_work_irects() -> Vec<(IRect, IRect)> {
    let mut list: Vec<HMONITOR> = Vec::new();
    // SAFETY: plain enumeration and queries; `list` outlives the call.
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(monitor_proc), LPARAM(&mut list as *mut _ as isize));
    }
    list.into_iter()
        .filter_map(|m| {
            let mut info = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            // SAFETY: as above.
            unsafe { GetMonitorInfoW(m, &mut info) }
                .as_bool()
                .then(|| (irect(info.rcMonitor), irect(info.rcWork)))
        })
        .collect()
}

/// The rectangle a framed output takes when no usable rectangle is remembered (a first start): the work area of
/// a monitor, the whole screen without the taskbar, as a normal framed window (the same "maximised size" as a snap
/// to the top edge, [`snap_rect`]). The monitor is `index` (`--monitor`) when it exists, else the one whose whole
/// rectangle is `near` (the monitor of the source), else the first. `monitors` lists (monitor, work area). Pure.
pub fn default_work_area(monitors: &[(IRect, IRect)], index: Option<usize>, near: Option<IRect>) -> Option<IRect> {
    index
        .and_then(|i| monitors.get(i))
        .or_else(|| near.and_then(|n| monitors.iter().find(|(m, _)| *m == n)))
        .or_else(|| monitors.first())
        .map(|(_, work)| *work)
}

/// How close (px) to an outer screen edge the pointer must be released for [`snap_rect`].
pub const SNAP_EDGE: i32 = 2;

/// The self-run title-bar drag of the framed output disables Windows' snap (its modal move loop activates the
/// window): a simple replacement. Released with the pointer at the left or right outer edge of a monitor, the
/// window takes that half of the monitor's work area; at the top edge, the whole work area (a maximised size, the
/// window stays a normal framed one). An edge shared with another monitor does not snap (the pointer crosses it).
/// `monitors` lists (monitor, work area). `None`: no snap. Pure.
pub fn snap_rect(cursor: (i32, i32), monitors: &[(IRect, IRect)]) -> Option<IRect> {
    let inside = |r: &IRect, (x, y): (i32, i32)| x >= r.left && x < r.right && y >= r.top && y < r.bottom;
    let (mon, work) = monitors.iter().find(|(m, _)| inside(m, cursor))?;
    let outer = |p: (i32, i32)| !monitors.iter().any(|(m, _)| inside(m, p));
    let (x, y) = cursor;
    let half = work.left + work.width() / 2;
    if y < mon.top + SNAP_EDGE && outer((x, mon.top - SNAP_EDGE)) {
        Some(*work)
    } else if x < mon.left + SNAP_EDGE && outer((mon.left - SNAP_EDGE, y)) {
        Some(IRect::new(work.left, work.top, half, work.bottom))
    } else if x >= mon.right - SNAP_EDGE && outer((mon.right - 1 + SNAP_EDGE, y)) {
        Some(IRect::new(half, work.top, work.right, work.bottom))
    } else {
        None
    }
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

/// The extended style a new output window is created with: the mode's [`base_ex_style`] with `WS_EX_NOACTIVATE`
/// from the flag [`sync_activation`] just set (click-through is applied after the creation, with its layered
/// attributes).
fn created_ex_style(fullscreen: bool) -> isize {
    let no_activate = NO_ACTIVATE.load(Ordering::Relaxed);
    styled(
        base_ex_style(fullscreen),
        Behaviour {
            no_activate,
            click_through: false,
        },
    )
}

pub fn create(opts: &Options) -> Result<Output> {
    // Every mode starts non-activating (OUT-2, OUT-3), the default framed window included: starting, clicking or
    // dragging the output leaves the foreground and the keyboard focus on the captured window. Only the settings
    // menu activates it.
    MODE_NO_ACTIVATE.store(true, Ordering::Relaxed);
    DESTROYING.store(false, Ordering::Relaxed);
    IN_SIZE_MOVE.store(false, Ordering::Relaxed);
    MENU_OPEN.store(false, Ordering::Relaxed);
    CLICK_THROUGH.store(opts.click_through, Ordering::Relaxed);
    // No window yet: only the flag the window procedure reads during the creation.
    sync_activation(HWND::default());
    set_source(opts.follow);
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
            // Black, never the default white: what the client area shows before the first frame is presented.
            hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
            lpszClassName: w!("phoxium_output"),
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
                WINDOW_EX_STYLE(created_ex_style(true) as u32),
                w!("phoxium_output"),
                &HSTRING::from("phoxium"),
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
            let ex = WINDOW_EX_STYLE(created_ex_style(false) as u32);
            // T-57: the rectangle of the last windowed run, when it is still on a monitor.
            let (x, y, w, h) = match opts.saved_rect.filter(|s| modes::usable_rect(s, &monitor_irects())) {
                Some(s) => (s.left, s.top, s.width(), s.height()),
                // A first start: the work area of the source's monitor (the size of a maximised window, still a
                // normal framed window), else the size Windows picks for `client_size`.
                None => {
                    let near = (!opts.follow.is_invalid())
                        .then(|| monitor_rect(MonitorFromWindow(opts.follow, MONITOR_DEFAULTTONEAREST)))
                        .flatten()
                        .map(irect);
                    match default_work_area(&monitor_work_irects(), opts.monitor, near) {
                        Some(w) => (w.left, w.top, w.width(), w.height()),
                        None => (CW_USEDEFAULT, CW_USEDEFAULT, r.right - r.left, r.bottom - r.top),
                    }
                }
            };
            CreateWindowExW(
                ex,
                w!("phoxium_output"),
                &HSTRING::from("phoxium"),
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
        let mut registration = hotkeys::register_all(&mut registrar, &hotkeys::plan_registration(&opts.hotkeys));
        // A menu key other than its default keeps the documented fallback registered too.
        hotkeys::update_menu_fallback(&mut registrar, &mut registration);
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
            let tried = registration
                .outcome(Action::Quit)
                .map(|o| {
                    o.failures
                        .iter()
                        .map(|f| f.0.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            return Err(windows::core::Error::new(
                HRESULT::from_win32(code),
                format!(
                    "cannot register the quit hotkey ({tried}: another program owns it); refusing to go fullscreen"
                ),
            ));
        }
        FULLSCREEN.store(opts.fullscreen, Ordering::Relaxed);
        OWN.store(hwnd.0 as isize, Ordering::Relaxed);
        sync_activation(hwnd);
        // Not shown yet: the application shows it once its first frame is presented ([`reveal`]), so no empty
        // client area flashes at start.
        REVEALED.store(false, Ordering::Relaxed);
        ERASE_BLACK.store(true, Ordering::Relaxed);
        Ok(Output {
            hwnd,
            hinstance: hinstance.0 as isize,
            hotkeys: registration,
        })
    }
}

/// Shows the output window the first time it is called (later calls do nothing): after the first presented frame,
/// so the window appears with a picture and never with an empty client area. Non-activating as every mode
/// ([`sync_activation`] already set the styles): `SW_SHOWNOACTIVATE` unless the output may take the foreground.
pub fn reveal(hwnd: HWND) {
    if REVEALED.swap(true, Ordering::Relaxed) {
        return;
    }
    // SAFETY: plain Win32 calls on our own window handle.
    unsafe {
        if NO_ACTIVATE.load(Ordering::Relaxed) {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        } else {
            // The menu (or the picker) opened before the first frame and asked for the keyboard while the window
            // was hidden: shown active, and the foreground asked again now that it can be given.
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
        }
    }
}

impl Output {
    /// Rebinds `action` to `chord` now (the menu's Hotkeys rows): `hotkeys::rebind` on this window's
    /// registrations. `Ok`: the new chord is in force; `Err`: the old one is kept (the message says why, with the
    /// Win32 reason when the system refused the chord).
    pub fn rebind(&mut self, action: Action, chord: Chord) -> std::result::Result<String, String> {
        hotkeys::rebind(
            &mut Win32Registrar { hwnd: self.hwnd },
            &mut self.hotkeys,
            action,
            chord,
        )
    }

    /// Every action back to its default chord now ("Reset all hotkeys", "Reset everything"). Returns the problems.
    pub fn reset_hotkeys(&mut self) -> Vec<String> {
        hotkeys::reset_all(&mut Win32Registrar { hwnd: self.hwnd }, &mut self.hotkeys)
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

/// The quit paths (the Quit hotkey, the close button): hide the window and leave the message loop. The window is not
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
        // activate the window and give it the keyboard focus (OUT-2). Windows' snap does not apply (a simple edge snap on
        // release replaces it, `snap_rect`), nor does the keyboard move.
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
            let hit = DRAG.get().map(|d| d.hit);
            DRAG.set(None);
            let _ = ReleaseCapture();
            // A title-bar move released at an outer screen edge snaps (half or whole work area, `snap_rect`).
            if hit == Some(HT_CAPTION) {
                let mut p = windows::Win32::Foundation::POINT::default();
                let _ = GetCursorPos(&mut p);
                if let Some(r) = snap_rect((p.x, p.y), &monitor_work_irects()) {
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
            }
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
        // Until the first frame is shown the client area is filled black (never the white of an unpainted window),
        // and once more at the first erase after the window was shown (whatever the compositor shows before the
        // swapchain's frame is black); after that the renderer covers it and erasing would only flicker.
        WM_ERASEBKGND => {
            if ERASE_BLACK.load(Ordering::Relaxed) {
                if REVEALED.load(Ordering::Relaxed) {
                    ERASE_BLACK.store(false, Ordering::Relaxed);
                }
                let mut rc = RECT::default();
                if GetClientRect(hwnd, &mut rc).is_ok() {
                    FillRect(
                        HDC(wparam.0 as *mut std::ffi::c_void),
                        &rc,
                        HBRUSH(GetStockObject(BLACK_BRUSH).0),
                    );
                }
            }
            LRESULT(1)
        }
        // The close button (and Alt+F4 while the menu has the focus): the same path as the Quit hotkey.
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
                // LPARAM: the MOD_* flags in the low word, the virtual key in the high word.
                let chord = Chord {
                    mods: hotkeys::Mods::from_bits(lparam.0 as u32 & 0xFFFF),
                    vk: ((lparam.0 as usize >> 16) & 0xFFFF) as u16,
                };
                ACTIONS.lock().unwrap_or_else(|e| e.into_inner()).push((action, chord));
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
            if let Some(mut d) = DRAG.get() {
                d.start = rebase_drag_on_dpi(d.hit, d.start, irect(*r));
                DRAG.set(Some(d));
            }
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
        base_ex_style, behaviour, default_work_area, drag_rect, effective_no_activate, hand_back_target, on_activate,
        rebase_drag_on_dpi, self_drag_hit, should_idle, snap_rect, styled, Behaviour, OnActivate,
    };
    use crate::modes::IRect;
    use windows::Win32::UI::WindowsAndMessaging::{
        WS_EX_APPWINDOW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT,
    };

    /// A first start opens as large as a maximised window: the work area of the chosen monitor, else the source's,
    /// else the first.
    #[test]
    fn the_default_framed_rectangle_is_the_work_area_of_a_monitor() {
        let m0 = (IRect::new(0, 0, 2560, 1440), IRect::new(0, 0, 2560, 1400));
        let m1 = (IRect::new(2560, 0, 5120, 1440), IRect::new(2560, 0, 5120, 1400));
        let both = [m0, m1];
        assert_eq!(default_work_area(&both, None, None), Some(m0.1), "the first monitor");
        assert_eq!(
            default_work_area(&both, None, Some(m1.0)),
            Some(m1.1),
            "the source's monitor"
        );
        assert_eq!(
            default_work_area(&both, Some(1), Some(m0.0)),
            Some(m1.1),
            "--monitor wins"
        );
        assert_eq!(
            default_work_area(&both, Some(7), Some(m1.0)),
            Some(m1.1),
            "a missing index falls back"
        );
        assert_eq!(
            default_work_area(&both, None, Some(IRect::new(9, 9, 10, 10))),
            Some(m0.1)
        );
        assert_eq!(default_work_area(&[], None, None), None, "no monitor: Windows decides");
    }

    /// Click-through state machine: menu open x click-through x mode. The open menu always takes the mouse and
    /// the activation back (the user can always reach it); closing it returns to the setting; the mode does not
    /// change click-through.
    /// Keep-awake asks Windows for `ES_CONTINUOUS | ES_DISPLAY_REQUIRED | ES_SYSTEM_REQUIRED` (0x80000003, the
    /// documented values 0x80000000, 2 and 1); released, `ES_CONTINUOUS` alone (0x80000000) clears the request.
    #[test]
    fn keep_awake_asks_for_the_documented_flags() {
        assert_eq!(super::keep_awake_flags(true), 0x8000_0003);
        assert_eq!(super::keep_awake_flags(false), 0x8000_0000);
    }

    #[test]
    fn click_through_follows_the_setting_and_the_open_menu_always_wins() {
        for mode in [true, false] {
            for click in [true, false] {
                for menu in [true, false] {
                    let b = behaviour(mode, click, menu);
                    assert_eq!(b.click_through, click && !menu, "mode {mode} click {click} menu {menu}");
                    assert_eq!(b.no_activate, effective_no_activate(mode, menu));
                    if menu {
                        assert!(
                            !b.click_through && !b.no_activate,
                            "the open menu takes the mouse and the focus"
                        );
                    }
                }
            }
        }
        // A session: on, the menu opens (mouse back), closes (click-through back), off.
        let steps = [(true, false), (true, true), (true, false), (false, false)];
        let seen: Vec<bool> = steps
            .iter()
            .map(|&(c, m)| behaviour(true, c, m).click_through)
            .collect();
        assert_eq!(seen, [true, false, true, false]);
    }

    /// The style bits: only NOACTIVATE, TRANSPARENT and LAYERED change, the others (topmost, app window) stay;
    /// TRANSPARENT never goes without LAYERED (the mouse would not pass to other programs' windows).
    #[test]
    fn the_style_bits_follow_the_behaviour_and_keep_the_others() {
        let base = (WS_EX_TOPMOST.0 | WS_EX_APPWINDOW.0) as isize;
        let on = styled(
            base,
            Behaviour {
                no_activate: true,
                click_through: true,
            },
        );
        for bit in [
            WS_EX_TOPMOST,
            WS_EX_APPWINDOW,
            WS_EX_NOACTIVATE,
            WS_EX_TRANSPARENT,
            WS_EX_LAYERED,
        ] {
            assert_ne!(on & bit.0 as isize, 0, "{bit:?}");
        }
        let off = styled(
            on,
            Behaviour {
                no_activate: false,
                click_through: false,
            },
        );
        assert_eq!(off, base);
        // Idempotent.
        let b = behaviour(true, true, false);
        assert_eq!(styled(styled(0, b), b), styled(0, b));
        for menu in [true, false] {
            let ex = styled(0, behaviour(true, true, menu));
            assert_eq!(
                ex & WS_EX_TRANSPARENT.0 as isize != 0,
                ex & WS_EX_LAYERED.0 as isize != 0
            );
        }
    }

    /// The taskbar button (owner report 2026-11-21): in both modes and every behaviour (non-activating,
    /// click-through, menu open) the style keeps `WS_EX_APPWINDOW` and never has `WS_EX_TOOLWINDOW`, so the shell
    /// shows a button although the window is `WS_EX_NOACTIVATE`. Fullscreen alone is topmost.
    #[test]
    fn every_mode_and_behaviour_keeps_a_taskbar_button() {
        for fullscreen in [true, false] {
            let base = base_ex_style(fullscreen);
            assert_eq!(base & WS_EX_TOPMOST.0 as isize != 0, fullscreen);
            for mode in [true, false] {
                for click in [true, false] {
                    for menu in [true, false] {
                        let b = behaviour(mode, click, menu);
                        let ex = styled(base, b);
                        let what = format!("fullscreen {fullscreen} mode {mode} click {click} menu {menu}");
                        assert_ne!(ex & WS_EX_APPWINDOW.0 as isize, 0, "{what}");
                        assert_eq!(ex & WS_EX_TOOLWINDOW.0 as isize, 0, "{what}");
                        assert_eq!(ex & WS_EX_NOACTIVATE.0 as isize != 0, b.no_activate, "{what}");
                        assert_eq!(ex & WS_EX_TOPMOST.0 as isize != 0, fullscreen, "{what}");
                    }
                }
            }
        }
        // The default start (non-activating, menu closed) is still non-activating in both modes.
        for fullscreen in [true, false] {
            let ex = styled(base_ex_style(fullscreen), behaviour(true, false, false));
            assert_ne!(ex & WS_EX_NOACTIVATE.0 as isize, 0);
        }
    }

    /// The edge snap of the self-run drag: left and right outer edges give the halves of the work area, the top
    /// edge the whole work area; a shared edge between two monitors and the middle of a monitor do not snap.
    #[test]
    fn a_drag_released_at_an_outer_edge_snaps_to_a_half_or_the_work_area() {
        let m0 = (IRect::new(0, 0, 2560, 1440), IRect::new(0, 0, 2560, 1400));
        let m1 = (IRect::new(2560, 0, 5120, 1440), IRect::new(2560, 0, 5120, 1440));
        let both = [m0, m1];
        assert_eq!(
            snap_rect((0, 700), &both),
            Some(IRect::new(0, 0, 1280, 1400)),
            "left half"
        );
        assert_eq!(
            snap_rect((5119, 700), &both),
            Some(IRect::new(3840, 0, 5120, 1440)),
            "right half"
        );
        assert_eq!(snap_rect((1000, 0), &both), Some(m0.1), "top: the work area");
        assert_eq!(snap_rect((3000, 0), &both), Some(m1.1));
        // The shared edge: the pointer crosses it, no snap on either side.
        assert_eq!(snap_rect((2559, 700), &both), None);
        assert_eq!(snap_rect((2560, 700), &both), None);
        // The middle, near but not at an edge, or off every monitor: no snap.
        assert_eq!(snap_rect((1280, 700), &both), None);
        assert_eq!(snap_rect((5, 700), &both), None);
        assert_eq!(snap_rect((-50, 700), &both), None);
        // One monitor: both sides are outer edges.
        assert_eq!(snap_rect((2559, 700), &[m0]), Some(IRect::new(1280, 0, 2560, 1400)));
    }

    /// A title-bar drag that crosses onto a monitor of another DPI takes the size Windows suggests; a frame resize
    /// keeps its start.
    #[test]
    fn a_drag_across_a_dpi_change_keeps_the_suggested_size() {
        let start = IRect::new(100, 100, 900, 700);
        let suggested = IRect::new(2600, 90, 3800, 990);
        assert_eq!(
            rebase_drag_on_dpi(2, start, suggested),
            IRect::new(100, 100, 1300, 1000)
        );
        assert_eq!(rebase_drag_on_dpi(17, start, suggested), start);
        // The next pointer move: the new size at the moved position.
        let r = drag_rect(2, rebase_drag_on_dpi(2, start, suggested), 2500, 0, (136, 39));
        assert_eq!((r.width(), r.height()), (1200, 900));
    }

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
