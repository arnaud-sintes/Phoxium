//! pixel-demo: a tiny Win32 window showing a low-resolution pixel-art source at an integer scale
//! (nearest neighbour). It is the capture target used to develop ultimatecrt.
//!
//! Keys: Space cycles scenes, 1-9 sets the integer scale, Esc quits.
//! Usage: pixel-demo [--res WxH] [--scale N] [--image file.png] [--scene scene|card|image]

mod art;

use std::{cell::RefCell, env, ffi::c_void, mem::size_of, time::Instant};

use windows::{
    core::{w, HSTRING},
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::Gdi::{
            BeginPaint, EndPaint, InvalidateRect, SetStretchBltMode, StretchDIBits, BITMAPINFO, BITMAPINFOHEADER,
            COLORONCOLOR, DIB_RGB_COLORS, PAINTSTRUCT, SRCCOPY,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2},
            WindowsAndMessaging::*,
        },
    },
};

#[derive(Clone, Copy, PartialEq)]
enum Content {
    Scene,
    Card,
    Image,
}

struct App {
    w: usize,
    h: usize,
    scale: usize,
    scenes: Vec<Content>,
    current: usize,
    image: Option<art::Image>,
    buf: Vec<u32>,
    start: Instant,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

const STYLE: WINDOW_STYLE = WINDOW_STYLE(WS_OVERLAPPED.0 | WS_CAPTION.0 | WS_SYSMENU.0 | WS_MINIMIZEBOX.0);

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let mut w = 256usize;
    let mut h = 240usize;
    let mut scale = 3usize;
    let mut image_path: Option<String> = None;
    let mut first = "scene".to_string();
    let mut res_given = false;
    let mut i = 0;
    while i < args.len() {
        let next = args.get(i + 1).cloned();
        match args[i].as_str() {
            "--res" => {
                let v = next.unwrap_or_default();
                let (a, b) = v.split_once('x').unwrap_or(("", ""));
                match (a.parse(), b.parse()) {
                    (Ok(a), Ok(b)) if a >= 16 && b >= 16 => {
                        w = a;
                        h = b;
                        res_given = true;
                    }
                    _ => fail("--res expects WxH, both at least 16"),
                }
                i += 1;
            }
            "--scale" => {
                scale = next
                    .and_then(|v| v.parse().ok())
                    .filter(|s| (1..=9).contains(s))
                    .unwrap_or_else(|| fail("--scale expects 1 to 9"));
                i += 1;
            }
            "--image" => {
                image_path = Some(next.unwrap_or_else(|| fail("--image expects a path")));
                i += 1;
            }
            "--scene" => {
                first = next.unwrap_or_default();
                i += 1;
            }
            "--version" => {
                println!("pixel-demo {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            "-h" | "--help" => {
                println!("pixel-demo [--res WxH] [--scale 1-9] [--image file.png] [--scene scene|card|image]");
                println!("keys: Space = next scene, 1-9 = scale, Esc = quit");
                return;
            }
            other => fail(&format!("unknown argument {other}")),
        }
        i += 1;
    }

    let image = image_path
        .as_deref()
        .map(|p| art::load_png(p).unwrap_or_else(|e| fail(&e)));
    if let (Some(img), false) = (&image, res_given) {
        w = img.w;
        h = img.h;
    }
    let mut scenes = vec![Content::Scene, Content::Card];
    if image.is_some() {
        scenes.push(Content::Image);
    }
    let current = match first.as_str() {
        "card" => 1,
        "image" if image.is_some() => 2,
        _ => 0,
    };
    println!(
        "pixel-demo: source {w}x{h}, scale {scale}x, window client {}x{}",
        w * scale,
        h * scale
    );

    unsafe {
        // Physical pixels everywhere so that the captured client area is exactly w*scale by h*scale.
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

        APP.with(|a| {
            *a.borrow_mut() = Some(App {
                w,
                h,
                scale,
                scenes,
                current,
                image,
                buf: vec![0; w * h],
                start: Instant::now(),
            });
        });

        let (ww, wh) = window_size(w * scale, h * scale);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("pixel_demo_window"),
            &HSTRING::from("pixel-demo"),
            STYLE,
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
        update_title(hwnd);
        let _ = ShowWindow(hwnd, SW_SHOW);
        SetTimer(Some(hwnd), 1, 8, None);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn fail(msg: &str) -> ! {
    eprintln!("pixel-demo: {msg}");
    std::process::exit(2);
}

/// Outer window size for a given client size.
unsafe fn window_size(cw: usize, ch: usize) -> (i32, i32) {
    let mut r = RECT {
        left: 0,
        top: 0,
        right: cw as i32,
        bottom: ch as i32,
    };
    let _ = AdjustWindowRectEx(&mut r, STYLE, false, WINDOW_EX_STYLE(0));
    (r.right - r.left, r.bottom - r.top)
}

fn update_title(hwnd: HWND) {
    let title = APP.with(|a| {
        let a = a.borrow();
        let a = a.as_ref().unwrap();
        let name = match a.scenes[a.current] {
            Content::Scene => "scene",
            Content::Card => "card",
            Content::Image => "image",
        };
        format!("pixel-demo | {name} {}x{} x{}", a.w, a.h, a.scale)
    });
    unsafe {
        let _ = SetWindowTextW(hwnd, &HSTRING::from(title));
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER => {
            let _ = InvalidateRect(Some(hwnd), None, false);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let key = wparam.0 as u32;
            if key == VK_ESCAPE_CODE {
                let _ = DestroyWindow(hwnd);
            } else if key == VK_SPACE_CODE {
                APP.with(|a| {
                    let mut a = a.borrow_mut();
                    let a = a.as_mut().unwrap();
                    a.current = (a.current + 1) % a.scenes.len();
                });
                update_title(hwnd);
            } else if (0x31..=0x39).contains(&key) {
                let scale = (key - 0x30) as usize;
                let (cw, ch) = APP.with(|a| {
                    let mut a = a.borrow_mut();
                    let a = a.as_mut().unwrap();
                    a.scale = scale;
                    (a.w * scale, a.h * scale)
                });
                let (ww, wh) = window_size(cw, ch);
                let _ = SetWindowPos(hwnd, None, 0, 0, ww, wh, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
                update_title(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

const VK_ESCAPE_CODE: u32 = 0x1B;
const VK_SPACE_CODE: u32 = 0x20;

unsafe fn paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);
    APP.with(|a| {
        let mut a = a.borrow_mut();
        let a = a.as_mut().unwrap();
        // 60 Hz simulation clock, independent of the paint rate.
        let t = (a.start.elapsed().as_secs_f64() * 60.0) as u32;
        let (w, h) = (a.w, a.h);
        match a.scenes[a.current] {
            Content::Scene => art::draw_scene(&mut a.buf, w, h, t),
            Content::Card => art::draw_card(&mut a.buf, w, h, t),
            Content::Image => {
                if let Some(img) = &a.image {
                    art::draw_image(&mut a.buf, w, h, img);
                }
            }
        }
        let bmi = BITMAPINFO {
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
        };
        SetStretchBltMode(hdc, COLORONCOLOR);
        StretchDIBits(
            hdc,
            0,
            0,
            (w * a.scale) as i32,
            (h * a.scale) as i32,
            0,
            0,
            w as i32,
            h as i32,
            Some(a.buf.as_ptr() as *const c_void),
            &bmi,
            DIB_RGB_COLORS,
            SRCCOPY,
        );
    });
    let _ = EndPaint(hwnd, &ps);
}
