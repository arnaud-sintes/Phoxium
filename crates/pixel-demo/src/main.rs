//! pixel-demo: a Win32 window showing a low-resolution pixel-art source. It is the universal test
//! source used to develop ultimatecrt: 1:1, integer, auto-fit and filtered free-fit display,
//! centring with bars, window modes, DPI handling, several test contents with a frame counter,
//! and a high-resolution update clock. Run `pixel-demo --help` for the options and keys.

mod art;
mod clock;
mod counter;
mod layout;
mod options;
mod patterns;
mod resample;

use std::{
    cell::RefCell,
    env,
    ffi::c_void,
    mem::size_of,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use windows::{
    core::{w, HSTRING},
    Win32::{
        Foundation::{COLORREF, HANDLE, HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::{
            Dwm::{DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND},
            Gdi::{
                BeginPaint, CreateSolidBrush, EndPaint, FillRect, GetMonitorInfoW, InvalidateRect, MonitorFromWindow,
                SetDIBitsToDevice, SetStretchBltMode, StretchDIBits, UpdateWindow, BITMAPINFO, BITMAPINFOHEADER,
                COLORONCOLOR, DIB_RGB_COLORS, HBRUSH, MONITORINFO, MONITOR_DEFAULTTONEAREST, PAINTSTRUCT, SRCCOPY,
            },
        },
        System::{
            LibraryLoader::GetModuleHandleW,
            Threading::{CreateWaitableTimerExW, SetWaitableTimer},
        },
        UI::{
            HiDpi::{
                AdjustWindowRectExForDpi, GetDpiForSystem, GetDpiForWindow, SetProcessDpiAwarenessContext,
                DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
            },
            WindowsAndMessaging::*,
        },
    },
};

use layout::{Align, Layout, Par, ScaleMode};
use options::{Content, Options, Parsed};
use resample::Filter;

const VK_ESCAPE: u32 = 0x1B;
const VK_SPACE: u32 = 0x20;
const CREATE_WAITABLE_TIMER_HIGH_RESOLUTION: u32 = 0x2;
const TIMER_ALL_ACCESS: u32 = 0x1F_0003;

struct App {
    o: Options,
    nw: usize,
    nh: usize,
    contents: Vec<Content>,
    current: usize,
    image: Option<art::Image>,
    buf: Vec<u32>,
    frame: u64,
    start: Instant,
    seed_base: u32,
    /// Filtered copy of `buf` for the free-fit modes, valid for `scaled_dims` until the next render.
    scaled: Vec<u32>,
    scaled_dims: (usize, usize),
    scaled_valid: bool,
    brush: HBRUSH,
    /// Saved style and placement while fullscreen.
    saved: Option<(isize, WINDOWPLACEMENT)>,
    stat_start: Instant,
    stat_ticks: u32,
    stat_skipped: u64,
    rate: f64,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let mut o = match options::parse(&args) {
        Ok(Parsed::Run(o)) => *o,
        Ok(Parsed::Help) => {
            println!("{}", options::HELP);
            return;
        }
        Ok(Parsed::Version) => {
            println!("pixel-demo {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        Err(e) => fail(&e),
    };

    if let Some(path) = o.decode.clone() {
        std::process::exit(decode_capture(&o, &path));
    }

    let image = o
        .image
        .as_deref()
        .map(|p| art::load_png(p).unwrap_or_else(|e| fail(&e)));
    if let (Some(img), false) = (&image, o.native_given) {
        o.native = (img.w, img.h);
    }
    let (nw, nh) = o.native;
    let mut contents = vec![
        Content::Scene,
        Content::Card,
        Content::Bars,
        Content::Ramp,
        Content::Checker,
        Content::Motion,
        Content::Text,
        Content::Noise,
    ];
    if image.is_some() {
        contents.push(Content::Image);
    }
    let current = contents.iter().position(|c| *c == o.content).unwrap_or(0);

    let (cw, ch) = initial_client(&o);
    println!(
        "pixel-demo: source {nw}x{nh}, {}, content {}, {} Hz, initial client {cw}x{ch}",
        mode_text(&o),
        contents[current].name(),
        o.fps
    );

    let seed_base = if o.fixed_seed {
        0x00C0_FFEE
    } else {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos() ^ d.as_secs() as u32)
            .unwrap_or(1)
    };

    unsafe {
        // Physical pixels everywhere so that the captured client area is exactly what is laid out.
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let hinstance = GetModuleHandleW(None).expect("GetModuleHandleW");
        let class = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: w!("pixel_demo_window"),
            ..Default::default()
        };
        RegisterClassW(&class);

        let bgr = ((o.bg & 0xFF) << 16) | (o.bg & 0xFF00) | ((o.bg >> 16) & 0xFF);
        let now = Instant::now();
        let borderless = o.borderless;
        let (fullscreen, maximised, no_round, fps, exit_after, stats) = (
            o.fullscreen,
            o.maximised,
            o.no_round_corners,
            o.fps,
            o.exit_after,
            o.stats,
        );
        APP.with(|a| {
            let mut app = App {
                o,
                nw,
                nh,
                contents,
                current,
                image,
                buf: vec![0; nw * nh],
                frame: 0,
                start: now,
                seed_base,
                scaled: Vec::new(),
                scaled_dims: (0, 0),
                scaled_valid: false,
                brush: CreateSolidBrush(COLORREF(bgr)),
                saved: None,
                stat_start: now,
                stat_ticks: 0,
                stat_skipped: 0,
                rate: 0.0,
            };
            render_into(&mut app);
            *a.borrow_mut() = Some(app);
        });

        let style = base_style(borderless);
        let (ww, wh) = outer_size(cw, ch, style, GetDpiForSystem());
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("pixel_demo_window"),
            &HSTRING::from("pixel-demo"),
            style,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            ww,
            wh,
            None,
            None,
            Some(hinstance.into()),
            None,
        )
        .expect("CreateWindowExW");
        if no_round {
            // Windows 11 only: older systems reject the attribute, which is fine.
            let pref = DWMWCP_DONOTROUND;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &pref as *const _ as *const c_void,
                size_of::<i32>() as u32,
            );
        }
        // The window may have been created on a monitor with another DPI than the system one.
        if !maximised && !fullscreen {
            fit_window_to_client(hwnd, cw, ch);
        }
        update_title(hwnd);
        let _ = ShowWindow(hwnd, if maximised { SW_SHOWMAXIMIZED } else { SW_SHOW });
        if fullscreen {
            toggle_fullscreen(hwnd);
        }
        run_loop(hwnd, fps, exit_after, stats);
    }
}

fn fail(msg: &str) -> ! {
    eprintln!("pixel-demo: {msg}");
    std::process::exit(2);
}

fn mode_text(o: &Options) -> String {
    match o.mode {
        ScaleMode::Native => "mode native (1:1)".into(),
        ScaleMode::Integer(n) => format!("mode integer x{n}"),
        ScaleMode::Fit => "mode fit".into(),
        ScaleMode::FitFree => format!("mode fit-free ({})", o.filter.name()),
    }
}

/// Initial client size: `--window-size`, else the picture at its scale (3 for the fit modes).
fn initial_client(o: &Options) -> (u32, u32) {
    if let Some(s) = o.window_size {
        return s;
    }
    let (nw, nh) = (o.native.0 as u32, o.native.1 as u32);
    match o.mode {
        ScaleMode::Native => (nw, nh),
        ScaleMode::Integer(n) => layout::client_for_scale(nw, nh, o.par, n),
        ScaleMode::Fit | ScaleMode::FitFree => layout::client_for_scale(nw, nh, o.par, 3),
    }
}

/// Offline `--decode`: prints the frame counter of a client-area capture. Returns the exit code.
fn decode_capture(o: &Options, path: &str) -> i32 {
    let img = art::load_png(path).unwrap_or_else(|e| fail(&e));
    let (nw, nh) = o.native;
    let lay = layout::layout(img.w as u32, img.h as u32, nw as u32, nh as u32, o.par, o.mode, o.align);
    match counter::decode_frame(&img.px, img.w, img.h, lay.dest, nw, nh) {
        Some(v) => {
            println!(
                "counter {v} (capture {}x{}, picture {}x{} at {},{})",
                img.w, img.h, lay.dest.w, lay.dest.h, lay.dest.x, lay.dest.y
            );
            0
        }
        None => {
            println!("no counter found (capture {}x{})", img.w, img.h);
            1
        }
    }
}

fn base_style(borderless: bool) -> WINDOW_STYLE {
    if borderless {
        WS_POPUP
    } else {
        WS_OVERLAPPEDWINDOW
    }
}

/// Outer window size for a client size, at a DPI.
unsafe fn outer_size(cw: u32, ch: u32, style: WINDOW_STYLE, dpi: u32) -> (i32, i32) {
    let mut r = RECT {
        left: 0,
        top: 0,
        right: cw as i32,
        bottom: ch as i32,
    };
    let _ = AdjustWindowRectExForDpi(&mut r, style, false, WINDOW_EX_STYLE(0), dpi);
    (r.right - r.left, r.bottom - r.top)
}

/// Resizes the window (keeping its position) so that its client area is `cw x ch`, unless it is
/// maximised or fullscreen.
unsafe fn fit_window_to_client(hwnd: HWND, cw: u32, ch: u32) {
    let fullscreen = APP.with(|a| a.borrow().as_ref().is_some_and(|a| a.saved.is_some()));
    if fullscreen || IsZoomed(hwnd).as_bool() {
        return;
    }
    let style = WINDOW_STYLE(GetWindowLongPtrW(hwnd, GWL_STYLE) as u32);
    let (ww, wh) = outer_size(cw, ch, style, GetDpiForWindow(hwnd));
    let _ = SetWindowPos(hwnd, None, 0, 0, ww, wh, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
}

unsafe fn toggle_fullscreen(hwnd: HWND) {
    let restore = APP.with(|a| a.borrow_mut().as_mut().unwrap().saved.take());
    if let Some((style, placement)) = restore {
        SetWindowLongPtrW(hwnd, GWL_STYLE, style);
        let _ = SetWindowPlacement(hwnd, &placement);
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_FRAMECHANGED,
        );
        return;
    }
    let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
    let mut placement = WINDOWPLACEMENT {
        length: size_of::<WINDOWPLACEMENT>() as u32,
        ..Default::default()
    };
    let _ = GetWindowPlacement(hwnd, &mut placement);
    let mut mi = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if !GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &mut mi).as_bool() {
        return;
    }
    APP.with(|a| a.borrow_mut().as_mut().unwrap().saved = Some((style, placement)));
    SetWindowLongPtrW(
        hwnd,
        GWL_STYLE,
        ((style as u32) & !WS_OVERLAPPEDWINDOW.0 | WS_POPUP.0) as isize,
    );
    let r = mi.rcMonitor;
    let _ = SetWindowPos(
        hwnd,
        None,
        r.left,
        r.top,
        r.right - r.left,
        r.bottom - r.top,
        SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_FRAMECHANGED,
    );
}

fn counter_on(o: &Options, c: Content) -> bool {
    o.counter.unwrap_or(c == Content::Motion)
}

/// Draws the current content for the current frame into the native buffer.
fn render_into(a: &mut App) {
    let (w, h) = (a.nw, a.nh);
    let c = a.contents[a.current];
    let fps = a.o.fps;
    match c {
        Content::Scene => art::draw_scene(&mut a.buf, w, h, clock::art_time(a.frame, fps)),
        Content::Card => art::draw_card(&mut a.buf, w, h, clock::art_time(a.frame, fps)),
        Content::Bars => patterns::draw_bars(&mut a.buf, w, h),
        Content::Ramp => patterns::draw_ramp(&mut a.buf, w, h),
        Content::Checker => patterns::draw_checker(&mut a.buf, w, h),
        Content::Motion => patterns::draw_motion(&mut a.buf, w, h, a.frame, fps),
        Content::Text => patterns::draw_text(&mut a.buf, w, h),
        Content::Noise => patterns::draw_noise(&mut a.buf, a.seed_base ^ (a.frame as u32).wrapping_mul(0x9E37_79B1)),
        Content::Image => {
            if let Some(img) = &a.image {
                art::draw_image(&mut a.buf, w, h, img);
            }
        }
    }
    if counter_on(&a.o, c) {
        counter::draw(&mut a.buf, w, h, a.frame as u32);
    }
    a.scaled_valid = false;
}

fn update_title(hwnd: HWND) {
    let title = APP.with(|a| {
        let a = a.borrow();
        let a = a.as_ref().unwrap();
        let mode = match a.o.mode {
            ScaleMode::Native => "1:1".to_string(),
            ScaleMode::Integer(n) => format!("x{n}"),
            ScaleMode::Fit => "fit".to_string(),
            ScaleMode::FitFree => format!("fit-free {}", a.o.filter.name()),
        };
        let par = if a.o.par == Par::SQUARE {
            String::new()
        } else {
            format!(" par {}:{}", a.o.par.n, a.o.par.m)
        };
        format!(
            "pixel-demo | {} {}x{} {mode}{par} | {} Hz, {:.1}/s",
            a.contents[a.current].name(),
            a.nw,
            a.nh,
            a.o.fps,
            a.rate
        )
    });
    unsafe {
        let _ = SetWindowTextW(hwnd, &HSTRING::from(title));
    }
}

/// Waits on a high-resolution waitable timer armed for each frame deadline, and pumps messages.
unsafe fn run_loop(hwnd: HWND, fps: u32, exit_after: Option<f64>, stats: bool) {
    let timer = match CreateWaitableTimerExW(None, None, CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, TIMER_ALL_ACCESS) {
        Ok(t) => t,
        Err(_) => {
            // Before Windows 10 1803: a plain timer, about 15 ms granularity.
            eprintln!("pixel-demo: no high-resolution timer, falling back to a coarse one");
            CreateWaitableTimerExW(None, None, 0, TIMER_ALL_ACCESS).expect("CreateWaitableTimerExW")
        }
    };
    arm(timer, 0, fps, 0);
    let mut msg = MSG::default();
    loop {
        let r = MsgWaitForMultipleObjectsEx(Some(&[timer]), u32::MAX, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
        if r.0 == 0 {
            let elapsed = tick(hwnd, stats);
            if exit_after.is_some_and(|s| elapsed as f64 / 1e9 >= s) {
                let _ = DestroyWindow(hwnd);
            }
            let n = clock::frame_index(elapsed, fps);
            arm(timer, n + 1, fps, elapsed);
        }
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            if msg.message == WM_QUIT {
                return;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Arms the one-shot timer for the start of frame `n`, `elapsed` ns after the start.
unsafe fn arm(timer: HANDLE, n: u64, fps: u32, elapsed: u64) {
    let wait_ns = clock::deadline_ns(n, fps).saturating_sub(elapsed);
    let due: i64 = -((wait_ns / 100).max(1) as i64);
    let _ = SetWaitableTimer(timer, &due, 0, None, None, false);
}

/// One timer wake-up: advances to the frame of the current time, renders and paints it.
/// Returns the elapsed nanoseconds since the start.
unsafe fn tick(hwnd: HWND, stats: bool) -> u64 {
    let (elapsed, report) = APP.with(|a| {
        let mut a = a.borrow_mut();
        let a = a.as_mut().unwrap();
        let elapsed = a.start.elapsed().as_nanos() as u64;
        let n = clock::frame_index(elapsed, a.o.fps);
        if n != a.frame || a.stat_ticks == 0 {
            if n > a.frame + 1 && a.stat_ticks > 0 {
                a.stat_skipped += n - a.frame - 1;
            }
            a.frame = n;
            a.stat_ticks += 1;
            render_into(a);
        }
        let win = a.stat_start.elapsed().as_secs_f64();
        let report = if win >= 1.0 {
            a.rate = a.stat_ticks as f64 / win;
            let line = format!(
                "pixel-demo: target {} Hz, painted {:.1}/s, skipped {} frames in the last {:.1} s",
                a.o.fps, a.rate, a.stat_skipped, win
            );
            a.stat_start = Instant::now();
            a.stat_ticks = 0;
            a.stat_skipped = 0;
            Some(line)
        } else {
            None
        };
        (elapsed, report)
    });
    if let Some(line) = report {
        if stats {
            println!("{line}");
        }
        update_title(hwnd);
    }
    let _ = InvalidateRect(Some(hwnd), None, false);
    let _ = UpdateWindow(hwnd);
    elapsed
}

fn current_layout(a: &App, cw: u32, ch: u32) -> Layout {
    layout::layout(cw, ch, a.nw as u32, a.nh as u32, a.o.par, a.o.mode, a.o.align)
}

/// Client size that exactly shows the picture (no bars) in the current mode, if the mode has one.
fn exact_client(a: &App) -> Option<(u32, u32)> {
    let (nw, nh) = (a.nw as u32, a.nh as u32);
    match a.o.mode {
        ScaleMode::Native => Some((nw, nh)),
        ScaleMode::Integer(n) => Some(layout::client_for_scale(nw, nh, a.o.par, n)),
        _ => None,
    }
}

fn client_size(hwnd: HWND) -> (u32, u32) {
    let mut r = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut r);
    }
    (r.right.max(0) as u32, r.bottom.max(0) as u32)
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        WM_SIZE => {
            let _ = InvalidateRect(Some(hwnd), None, false);
            LRESULT(0)
        }
        WM_DPICHANGED => {
            on_dpi_changed(hwnd, wparam, lparam);
            LRESULT(0)
        }
        WM_KEYDOWN => {
            on_key(hwnd, wparam.0 as u32);
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// Microsoft's recommended handling: move to the suggested rectangle. When the window showed the
/// picture exactly (no bars), keep it exact at the new DPI instead, so that the client area stays
/// `width * scale` by `height * scale` and the captured pixels stay 1:1.
unsafe fn on_dpi_changed(hwnd: HWND, wparam: WPARAM, lparam: LPARAM) {
    let new_dpi = ((wparam.0 >> 16) & 0xFFFF) as u32;
    let suggested = *(lparam.0 as *const RECT);
    let (fullscreen, exact, current) = APP.with(|a| {
        let a = a.borrow();
        let a = a.as_ref().unwrap();
        (a.saved.is_some(), exact_client(a), client_size(hwnd))
    });
    if fullscreen {
        return;
    }
    let (x, y, mut ww, mut wh) = (
        suggested.left,
        suggested.top,
        suggested.right - suggested.left,
        suggested.bottom - suggested.top,
    );
    if let Some((cw, ch)) = exact {
        if (cw, ch) == current && !IsZoomed(hwnd).as_bool() {
            let style = WINDOW_STYLE(GetWindowLongPtrW(hwnd, GWL_STYLE) as u32);
            (ww, wh) = outer_size(cw, ch, style, new_dpi);
        }
    }
    let _ = SetWindowPos(hwnd, None, x, y, ww, wh, SWP_NOZORDER | SWP_NOACTIVATE);
}

unsafe fn on_key(hwnd: HWND, key: u32) {
    let mut resize: Option<(u32, u32)> = None;
    match key {
        VK_ESCAPE => {
            let _ = DestroyWindow(hwnd);
            return;
        }
        0x46 => {
            // F
            toggle_fullscreen(hwnd);
        }
        VK_SPACE => APP.with(|a| {
            let mut a = a.borrow_mut();
            let a = a.as_mut().unwrap();
            a.current = (a.current + 1) % a.contents.len();
            render_into(a);
        }),
        0x30..=0x39 => APP.with(|a| {
            let mut a = a.borrow_mut();
            let a = a.as_mut().unwrap();
            let n = if key == 0x30 { 10 } else { key - 0x30 };
            a.o.mode = ScaleMode::Integer(n);
            resize = exact_client(a);
        }),
        0x4D => APP.with(|a| {
            // M: native -> integer -> fit -> fit-free -> native
            let mut a = a.borrow_mut();
            let a = a.as_mut().unwrap();
            let last = match a.o.mode {
                ScaleMode::Integer(n) => n,
                _ => 3,
            };
            a.o.mode = match a.o.mode {
                ScaleMode::Native => ScaleMode::Integer(last),
                ScaleMode::Integer(_) => ScaleMode::Fit,
                ScaleMode::Fit => ScaleMode::FitFree,
                ScaleMode::FitFree => ScaleMode::Native,
            };
            resize = exact_client(a);
        }),
        0x42 => APP.with(|a| {
            // B
            let mut a = a.borrow_mut();
            let a = a.as_mut().unwrap();
            a.o.filter = match a.o.filter {
                Filter::Nearest => Filter::Bilinear,
                Filter::Bilinear => Filter::Bicubic,
                Filter::Bicubic => Filter::Nearest,
            };
            a.scaled_valid = false;
        }),
        0x41 => APP.with(|a| {
            // A
            let mut a = a.borrow_mut();
            let a = a.as_mut().unwrap();
            a.o.align = match a.o.align {
                Align::Center => Align::TopLeft,
                Align::TopLeft => Align::Center,
            };
        }),
        0x43 => APP.with(|a| {
            // C
            let mut a = a.borrow_mut();
            let a = a.as_mut().unwrap();
            let on = counter_on(&a.o, a.contents[a.current]);
            a.o.counter = Some(!on);
            render_into(a);
        }),
        _ => return,
    }
    if let Some((cw, ch)) = resize {
        fit_window_to_client(hwnd, cw, ch);
    }
    update_title(hwnd);
    let _ = InvalidateRect(Some(hwnd), None, false);
}

unsafe fn paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);
    let (cw, ch) = client_size(hwnd);
    APP.with(|a| {
        let mut a = a.borrow_mut();
        let a = a.as_mut().unwrap();
        let lay = current_layout(a, cw, ch);
        for b in &lay.bars {
            let r = RECT {
                left: b.x,
                top: b.y,
                right: b.right(),
                bottom: b.bottom(),
            };
            FillRect(hdc, &r, a.brush);
        }
        if lay.dest.is_empty() {
            return;
        }
        let filtered = a.o.mode == ScaleMode::FitFree && a.o.filter != Filter::Nearest;
        if filtered {
            let dims = (lay.dest.w as usize, lay.dest.h as usize);
            if !a.scaled_valid || a.scaled_dims != dims {
                a.scaled = resample::resample(&a.buf, a.nw, a.nh, dims.0, dims.1, a.o.filter);
                a.scaled_dims = dims;
                a.scaled_valid = true;
            }
            let bmi = dib_header(dims.0, dims.1);
            SetDIBitsToDevice(
                hdc,
                lay.dest.x,
                lay.dest.y,
                dims.0 as u32,
                dims.1 as u32,
                0,
                0,
                0,
                dims.1 as u32,
                a.scaled.as_ptr() as *const c_void,
                &bmi,
                DIB_RGB_COLORS,
            );
        } else {
            let bmi = dib_header(a.nw, a.nh);
            SetStretchBltMode(hdc, COLORONCOLOR);
            StretchDIBits(
                hdc,
                lay.dest.x,
                lay.dest.y,
                lay.dest.w,
                lay.dest.h,
                0,
                0,
                a.nw as i32,
                a.nh as i32,
                Some(a.buf.as_ptr() as *const c_void),
                &bmi,
                DIB_RGB_COLORS,
                SRCCOPY,
            );
        }
    });
    let _ = EndPaint(hwnd, &ps);
}

/// Top-down 32 bit DIB header.
fn dib_header(w: usize, h: usize) -> BITMAPINFO {
    BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w as i32,
            biHeight: -(h as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: 0,
            ..Default::default()
        },
        ..Default::default()
    }
}
