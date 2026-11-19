//! T-07 spike: the ultimatecrt output window (same styles, show call and pump as output.rs) with no
//! Vulkan and no capture. Options:
//!   --windowed        WS_OVERLAPPEDWINDOW + WS_EX_NOACTIVATE instead of the fullscreen popup
//!   --title           SetWindowTextW every second (as app.rs does)
//!   --print           a stdout line every second (as --stats does)
//!   --hotkey          RegisterHotKey Ctrl+Alt+Q (as output.rs does)
//!   --guard-pos       WM_WINDOWPOSCHANGING adds SWP_NOACTIVATE to every position change
//!   --refuse          WM_ACTIVATE (activated) hands the foreground back to the window it came from
//!   --exit-after <s>  quit after s seconds (default 40)
//! Logs every activation-related message it receives with its time since start.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use windows::{
    core::{w, HSTRING},
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTOPRIMARY},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Input::KeyboardAndMouse::{RegisterHotKey, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT},
            WindowsAndMessaging::*,
        },
    },
};

static mut START: Option<Instant> = None;
static GUARD_POS: AtomicBool = AtomicBool::new(false);
static REFUSE: AtomicBool = AtomicBool::new(false);
/// Last foreground window that was not ours, sampled by the loop.
static LAST_FG: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

fn t() -> f64 {
    unsafe { (*std::ptr::addr_of!(START)).map_or(0.0, |s| s.elapsed().as_secs_f64()) }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let name = match msg {
        WM_ACTIVATE => Some("WM_ACTIVATE"),
        WM_ACTIVATEAPP => Some("WM_ACTIVATEAPP"),
        WM_NCACTIVATE => Some("WM_NCACTIVATE"),
        WM_MOUSEACTIVATE => Some("WM_MOUSEACTIVATE"),
        WM_SETFOCUS => Some("WM_SETFOCUS"),
        WM_KILLFOCUS => Some("WM_KILLFOCUS"),
        WM_WINDOWPOSCHANGING => Some("WM_WINDOWPOSCHANGING"),
        WM_SHOWWINDOW => Some("WM_SHOWWINDOW"),
        _ => None,
    };
    if let Some(n) = name {
        let flags = if msg == WM_WINDOWPOSCHANGING {
            let wp = &*(lparam.0 as *const WINDOWPOS);
            format!(" flags=0x{:X}", wp.flags.0)
        } else {
            String::new()
        };
        println!("{:8.3} {n} wparam=0x{:X}{flags}", t(), wparam.0);
    }
    match msg {
        WM_WINDOWPOSCHANGING if GUARD_POS.load(Ordering::Relaxed) => {
            let wp = &mut *(lparam.0 as *mut WINDOWPOS);
            wp.flags |= SWP_NOACTIVATE;
            LRESULT(0)
        }
        WM_ACTIVATE if REFUSE.load(Ordering::Relaxed) && (wparam.0 & 0xFFFF) != 0 => {
            let mut prev = HWND(lparam.0 as *mut std::ffi::c_void);
            if prev.is_invalid() {
                prev = HWND(LAST_FG.load(Ordering::Relaxed) as *mut std::ffi::c_void);
            }
            let ok = !prev.is_invalid() && SetForegroundWindow(prev).as_bool();
            println!("{:8.3} refuse: SetForegroundWindow(prev {:?}) = {ok}", t(), prev.0);
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_ERASEBKGND => LRESULT(1),
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

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let has = |s: &str| args.iter().any(|a| a == s);
    let exit_after = args
        .iter()
        .position(|a| a == "--exit-after")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(40.0);
    GUARD_POS.store(has("--guard-pos"), Ordering::Relaxed);
    REFUSE.store(has("--refuse"), Ordering::Relaxed);
    unsafe {
        START = Some(Instant::now());
        if let Some(d) = args
            .iter()
            .position(|a| a == "--delay")
            .and_then(|i| args.get(i + 1))
            .and_then(|s| s.parse::<u64>().ok())
        {
            std::thread::sleep(Duration::from_secs(d));
            println!("{:8.3} delay over, creating the window", t());
        }
        let hinstance = GetModuleHandleW(None).unwrap();
        let class = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap(),
            lpszClassName: w!("focus_leak_output"),
            ..Default::default()
        };
        RegisterClassW(&class);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let _ = GetMonitorInfoW(
            MonitorFromPoint(Default::default(), MONITOR_DEFAULTTOPRIMARY),
            &mut info,
        );
        let r = info.rcMonitor;
        let hwnd = if has("--windowed") {
            CreateWindowExW(
                WS_EX_NOACTIVATE,
                w!("focus_leak_output"),
                &HSTRING::from("focus-leak"),
                WS_OVERLAPPEDWINDOW,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                800,
                600,
                None,
                None,
                Some(hinstance.into()),
                None,
            )
        } else {
            CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                w!("focus_leak_output"),
                &HSTRING::from("focus-leak"),
                WS_POPUP,
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top,
                None,
                None,
                Some(hinstance.into()),
                None,
            )
        }
        .unwrap();
        if has("--hotkey") {
            let _ = RegisterHotKey(Some(hwnd), 1, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, 'Q' as u32);
        }
        if has("--disabled") {
            let _ = windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow(hwnd, false);
        }
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        println!("{:8.3} shown", t());
        let mut last = Instant::now();
        let mut n = 0u32;
        'outer: loop {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    break 'outer;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            let fg = GetForegroundWindow();
            if !fg.is_invalid() && fg != hwnd {
                LAST_FG.store(fg.0 as isize, Ordering::Relaxed);
            }
            if t() > exit_after {
                break;
            }
            std::thread::sleep(Duration::from_millis(16));
            n += 1;
            if last.elapsed() >= Duration::from_secs(1) {
                last = Instant::now();
                if has("--title") {
                    let _ = SetWindowTextW(hwnd, &HSTRING::from(format!("focus-leak | frame {n}")));
                }
                if has("--print") {
                    println!("{:8.3} frame {n}", t());
                }
            }
        }
        println!("{:8.3} exit", t());
        let _ = DestroyWindow(hwnd);
    }
}
