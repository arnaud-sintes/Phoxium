//! T-08 live harness (prepared, NOT run by the build agent: it opens a window, registers global
//! hotkeys, moves the foreground and, with `--selftest`, synthesises key presses).
//!
//! An output-style window (windowed or borderless fullscreen, topmost, `WS_EX_NOACTIVATE` while the
//! menu is closed) presents a dark background, a read-only ImGui stats overlay and, while open, the
//! ImGui settings menu (D-7). Global hotkeys (RegisterHotKey): F11 toggles fullscreen and resizable
//! windowed, F12 opens and closes the menu. Opening remembers the foreground window (the source)
//! and activates the output window; input then goes to ImGui; closing gives the foreground back to
//! the remembered window. A poll thread logs every foreground change (GetForegroundWindow every
//! 10 ms) to a CSV together with the harness events. `--selftest N` drives N open/close cycles
//! itself with SendInput (F12, typing "t08" into the focused text field) and writes a summary.
//!
//! Usage: live [--fullscreen] [--selftest N] [--csv FILE] [--summary FILE] [--source-title TEXT]
//!             [--fg-method auto|plain|alt|attach] [--no-validation]
//!
//! Spike-only deviation from DESIGN.md ("no input synthesis"): the self-test and the `alt`
//! foreground workaround call SendInput. The product would keep only what the live run proves
//! necessary, and the self-test stays a test tool.

use std::cell::RefCell;
use std::fs::File;
use std::io::Write as _;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ash::{khr, vk};
use imgui_spike::gpu::{barrier, Context};
use imgui_spike::input::{self, UiEvent, Win32Input};
use imgui_spike::overlay::{self, Load, MenuState, Overlay};
use imgui_spike::Res;
use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::HiDpi::{
    GetDpiForWindow, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, RegisterHotKey, SendInput, SetFocus, UnregisterHotKey, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT,
    KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MOD_NOREPEAT, VIRTUAL_KEY, VK_CONTROL, VK_F11, VK_F12,
    VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::*;

const HOTKEY_F11: i32 = 1;
const HOTKEY_F12: i32 = 2;
const WINDOWED_STYLE: WINDOW_STYLE = WS_OVERLAPPEDWINDOW;
const FULLSCREEN_STYLE: WINDOW_STYLE = WS_POPUP;
const EX_BASE: WINDOW_EX_STYLE = WINDOW_EX_STYLE(WS_EX_TOPMOST.0 | WS_EX_TOOLWINDOW.0);
const BACKGROUND: [f32; 4] = [0.02, 0.10, 0.12, 1.0];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FgMethod {
    Auto,
    Plain,
    Alt,
    Attach,
}

struct Args {
    fullscreen: bool,
    selftest: u32,
    csv: String,
    summary: String,
    source_title: String,
    method: FgMethod,
    validation: bool,
}

fn parse_args() -> Res<Args> {
    let mut a = Args {
        fullscreen: false,
        selftest: 0,
        csv: "t08-live.csv".into(),
        summary: "t08-live-summary.txt".into(),
        source_title: "pixel-demo".into(),
        method: FgMethod::Auto,
        validation: true,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = |name: &str| it.next().ok_or_else(|| format!("{name} expects a value"));
        match arg.as_str() {
            "--fullscreen" => a.fullscreen = true,
            "--selftest" => a.selftest = value("--selftest")?.parse()?,
            "--csv" => a.csv = value("--csv")?,
            "--summary" => a.summary = value("--summary")?,
            "--source-title" => a.source_title = value("--source-title")?,
            "--no-validation" => a.validation = false,
            "--fg-method" => {
                a.method = match value("--fg-method")?.as_str() {
                    "auto" => FgMethod::Auto,
                    "plain" => FgMethod::Plain,
                    "alt" => FgMethod::Alt,
                    "attach" => FgMethod::Attach,
                    other => return Err(format!("unknown --fg-method {other}").into()),
                }
            }
            other => return Err(format!("unknown argument {other}").into()),
        }
    }
    Ok(a)
}

/// Messages the window procedure queues for the main loop (no re-entrancy: the loop takes the
/// queue out before calling Win32 functions that send messages back synchronously).
#[derive(Default)]
struct Queue {
    hotkeys: Vec<i32>,
    events: Vec<UiEvent>,
    quit: bool,
    resized: bool,
}

thread_local! {
    static QUEUE: RefCell<Queue> = RefCell::new(Queue::default());
    static CONV: RefCell<Win32Input> = RefCell::new(Win32Input::new());
}

/// True while the menu is open (read by the window procedure and the poll thread).
static MENU_OPEN: AtomicBool = AtomicBool::new(false);

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_HOTKEY => {
            QUEUE.with(|q| q.borrow_mut().hotkeys.push(wparam.0 as i32));
            return LRESULT(0);
        }
        WM_MOUSEACTIVATE => {
            let r = if MENU_OPEN.load(Ordering::SeqCst) {
                MA_ACTIVATE
            } else {
                MA_NOACTIVATE
            };
            return LRESULT(r as isize);
        }
        WM_SIZE => QUEUE.with(|q| q.borrow_mut().resized = true),
        WM_CLOSE | WM_DESTROY => {
            QUEUE.with(|q| q.borrow_mut().quit = true);
            return LRESULT(0);
        }
        _ => {}
    }
    if MENU_OPEN.load(Ordering::SeqCst) || msg == input::WM_DPICHANGED {
        let mut out = Vec::new();
        let used = CONV.with(|c| c.borrow_mut().translate(msg, wparam.0, lparam.0, &mut out));
        if !out.is_empty() {
            QUEUE.with(|q| q.borrow_mut().events.extend(out));
        }
        let sys = msg == input::WM_SYSKEYDOWN || msg == input::WM_SYSKEYUP;
        if used && !sys && msg != input::WM_DPICHANGED {
            return LRESULT(0);
        }
    }
    // SAFETY: forwarding the unchanged message of this window procedure.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// CSV log shared by the main thread and the poll thread.
struct Log {
    file: Mutex<File>,
    start: Instant,
    source_title: String,
}

impl Log {
    fn row(&self, kind: &str, detail: &str) {
        let (fg, title) = foreground();
        let me = SELF_HWND.load(Ordering::SeqCst);
        let line = format!(
            "{},{},\"{}\",{:#x},\"{}\",{},{},{}\n",
            self.start.elapsed().as_millis(),
            kind,
            detail.replace('"', "'"),
            fg,
            title.replace('"', "'"),
            u8::from(fg == me && me != 0),
            u8::from(self.is_source(&title)),
            u8::from(MENU_OPEN.load(Ordering::SeqCst)),
        );
        if let Ok(mut f) = self.file.lock() {
            let _ = f.write_all(line.as_bytes());
            let _ = f.flush();
        }
    }

    fn is_source(&self, title: &str) -> bool {
        !self.source_title.is_empty() && title.to_lowercase().contains(&self.source_title.to_lowercase())
    }
}

static SELF_HWND: AtomicIsize = AtomicIsize::new(0);

fn hwnd_of(v: isize) -> HWND {
    HWND(v as *mut std::ffi::c_void)
}

/// Current foreground window handle and title.
fn foreground() -> (isize, String) {
    // SAFETY: plain queries on a window handle that may be stale; failures give an empty title.
    unsafe {
        let h = GetForegroundWindow();
        let mut buf = [0u16; 256];
        let n = GetWindowTextW(h, &mut buf).max(0) as usize;
        (h.0 as isize, String::from_utf16_lossy(&buf[..n]))
    }
}

fn send_keys(keys: &[(VIRTUAL_KEY, u16, KEYBD_EVENT_FLAGS)]) -> u32 {
    let inputs: Vec<INPUT> = keys
        .iter()
        .map(|&(vk, scan, flags)| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: scan,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        })
        .collect();
    // SAFETY: a slice of initialised INPUT structures and its element size.
    unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) }
}

fn tap(vk: VIRTUAL_KEY) -> u32 {
    send_keys(&[(vk, 0, KEYBD_EVENT_FLAGS(0)), (vk, 0, KEYEVENTF_KEYUP)])
}

fn type_text(s: &str) -> u32 {
    let keys: Vec<_> = s
        .encode_utf16()
        .flat_map(|u| {
            [
                (VIRTUAL_KEY(0), u, KEYEVENTF_UNICODE),
                (VIRTUAL_KEY(0), u, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP),
            ]
        })
        .collect();
    send_keys(&keys)
}

/// Brings `target` to the foreground with the chosen method; returns the method that worked or
/// "failed". `auto` tries plain SetForegroundWindow, then a synthetic Alt tap (which satisfies the
/// foreground lock's "last input event" rule), then AttachThreadInput to the foreground thread.
fn bring_to_front(target: HWND, method: FgMethod) -> &'static str {
    // The switch to another process's window may land a little later: poll for up to 250 ms.
    let ok = || {
        for _ in 0..25 {
            // SAFETY: plain query.
            if unsafe { GetForegroundWindow() } == target {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    };
    let plain = || {
        // SAFETY: plain call on a window handle; failure is reported by the check.
        unsafe {
            let _ = SetForegroundWindow(target);
        }
        ok()
    };
    let alt = || {
        tap(VK_MENU);
        plain()
    };
    let attach = || {
        // SAFETY: attaching the input queues of this thread and the foreground thread for the
        // duration of the call, detached again before returning.
        unsafe {
            let fg = GetForegroundWindow();
            let fg_thread = GetWindowThreadProcessId(fg, None);
            let me = GetCurrentThreadId();
            let attached = fg_thread != 0 && fg_thread != me && AttachThreadInput(me, fg_thread, true).as_bool();
            let _ = SetForegroundWindow(target);
            let _ = BringWindowToTop(target);
            let _ = SetFocus(Some(target));
            if attached {
                let _ = AttachThreadInput(me, fg_thread, false);
            }
        }
        ok()
    };
    match method {
        FgMethod::Plain => {
            if plain() {
                "plain"
            } else {
                "failed"
            }
        }
        FgMethod::Alt => {
            if alt() {
                "alt"
            } else {
                "failed"
            }
        }
        FgMethod::Attach => {
            if attach() {
                "attach"
            } else {
                "failed"
            }
        }
        FgMethod::Auto => {
            if plain() {
                "plain"
            } else if alt() {
                "alt"
            } else if attach() {
                "attach"
            } else {
                "failed"
            }
        }
    }
}

fn set_ex_style(hwnd: HWND, noactivate: bool) {
    let ex = if noactivate {
        EX_BASE.0 | WS_EX_NOACTIVATE.0
    } else {
        EX_BASE.0
    };
    // SAFETY: our own live window.
    unsafe {
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex as isize);
    }
}

/// Rectangle of the monitor that holds `hwnd`.
fn monitor_rect(hwnd: HWND) -> RECT {
    // SAFETY: plain queries with a properly sized MONITORINFO.
    unsafe {
        let m = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let _ = GetMonitorInfoW(m, &mut info);
        info.rcMonitor
    }
}

/// Switches between borderless fullscreen over the window's monitor and a resizable window,
/// without activating it.
fn set_fullscreen(hwnd: HWND, on: bool) {
    let (style, r) = if on {
        (FULLSCREEN_STYLE, monitor_rect(hwnd))
    } else {
        let m = monitor_rect(hwnd);
        (
            WINDOWED_STYLE,
            RECT {
                left: m.left + 100,
                top: m.top + 100,
                right: m.left + 1060,
                bottom: m.top + 700,
            },
        )
    };
    // SAFETY: our own live window.
    unsafe {
        SetWindowLongPtrW(hwnd, GWL_STYLE, (style.0 | WS_VISIBLE.0) as isize);
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            r.left,
            r.top,
            r.right - r.left,
            r.bottom - r.top,
            SWP_NOACTIVATE | SWP_FRAMECHANGED | SWP_SHOWWINDOW,
        );
    }
}

fn client_size(hwnd: HWND) -> (u32, u32) {
    let mut r = RECT::default();
    // SAFETY: our own live window.
    let _ = unsafe { GetClientRect(hwnd, &mut r) };
    ((r.right - r.left).max(0) as u32, (r.bottom - r.top).max(0) as u32)
}

fn modifiers_now() -> input::Modifiers {
    // SAFETY: plain key-state queries.
    let down = |vk: VIRTUAL_KEY| unsafe { GetKeyState(i32::from(vk.0)) } < 0;
    input::Modifiers {
        ctrl: down(VK_CONTROL),
        shift: down(VK_SHIFT),
        alt: down(VK_MENU),
        win: down(VK_LWIN) || down(VK_RWIN),
    }
}

/// Swapchain on the output window (FIFO, B8G8R8A8_UNORM), one frame in flight.
struct Presenter {
    surface_fn: khr::surface::Instance,
    swapchain_fn: khr::swapchain::Device,
    surface: vk::SurfaceKHR,
    swapchain: vk::SwapchainKHR,
    images: Vec<vk::Image>,
    views: Vec<vk::ImageView>,
    extent: vk::Extent2D,
    acquire: vk::Semaphore,
    done: Vec<vk::Semaphore>,
    fence: vk::Fence,
    pool: vk::CommandPool,
    cb: vk::CommandBuffer,
    queries: vk::QueryPool,
    queried: bool,
}

const FORMAT: vk::Format = vk::Format::B8G8R8A8_UNORM;

impl Presenter {
    /// # Safety
    /// `surface` belongs to `ctx.instance` and its window; the presenter is destroyed before `ctx`.
    unsafe fn new(ctx: &Context, surface: vk::SurfaceKHR, size: (u32, u32)) -> Res<Self> {
        let d = &ctx.device;
        let pool = d.create_command_pool(
            &vk::CommandPoolCreateInfo::default()
                .queue_family_index(ctx.queue_family)
                .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
            None,
        )?;
        let cb = d.allocate_command_buffers(
            &vk::CommandBufferAllocateInfo::default()
                .command_pool(pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(1),
        )?[0];
        let mut p = Self {
            surface_fn: khr::surface::Instance::new(&ctx.entry, &ctx.instance),
            swapchain_fn: khr::swapchain::Device::new(&ctx.instance, d),
            surface,
            swapchain: vk::SwapchainKHR::null(),
            images: Vec::new(),
            views: Vec::new(),
            extent: vk::Extent2D::default(),
            acquire: d.create_semaphore(&vk::SemaphoreCreateInfo::default(), None)?,
            done: Vec::new(),
            fence: d.create_fence(
                &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
                None,
            )?,
            pool,
            cb,
            queries: d.create_query_pool(
                &vk::QueryPoolCreateInfo::default()
                    .query_type(vk::QueryType::TIMESTAMP)
                    .query_count(2),
                None,
            )?,
            queried: false,
        };
        p.recreate(ctx, size)?;
        Ok(p)
    }

    /// (Re)creates the swapchain for the current surface size; the device must be idle.
    unsafe fn recreate(&mut self, ctx: &Context, size: (u32, u32)) -> Res<()> {
        let d = &ctx.device;
        let caps = self
            .surface_fn
            .get_physical_device_surface_capabilities(ctx.physical, self.surface)?;
        let formats = self
            .surface_fn
            .get_physical_device_surface_formats(ctx.physical, self.surface)?;
        let format = formats
            .iter()
            .find(|f| f.format == FORMAT)
            .copied()
            .ok_or("surface does not offer B8G8R8A8_UNORM")?;
        let extent = if caps.current_extent.width != u32::MAX {
            caps.current_extent
        } else {
            vk::Extent2D {
                width: size.0,
                height: size.1,
            }
        };
        if extent.width == 0 || extent.height == 0 {
            return Ok(()); // minimised: keep the old swapchain, skip frames
        }
        let count = (caps.min_image_count + 1).min(if caps.max_image_count == 0 {
            u32::MAX
        } else {
            caps.max_image_count
        });
        let old = self.swapchain;
        let new = self.swapchain_fn.create_swapchain(
            &vk::SwapchainCreateInfoKHR::default()
                .surface(self.surface)
                .min_image_count(count)
                .image_format(format.format)
                .image_color_space(format.color_space)
                .image_extent(extent)
                .image_array_layers(1)
                .image_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT)
                .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
                .pre_transform(caps.current_transform)
                .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
                .present_mode(vk::PresentModeKHR::FIFO)
                .clipped(true)
                .old_swapchain(old),
            None,
        )?;
        self.destroy_swapchain_objects(d);
        if old != vk::SwapchainKHR::null() {
            self.swapchain_fn.destroy_swapchain(old, None);
        }
        self.swapchain = new;
        self.extent = extent;
        self.images = self.swapchain_fn.get_swapchain_images(new)?;
        for &img in &self.images {
            self.views.push(
                d.create_image_view(
                    &vk::ImageViewCreateInfo::default()
                        .image(img)
                        .view_type(vk::ImageViewType::TYPE_2D)
                        .format(FORMAT)
                        .subresource_range(imgui_spike::gpu::color_range()),
                    None,
                )?,
            );
            self.done
                .push(d.create_semaphore(&vk::SemaphoreCreateInfo::default(), None)?);
        }
        Ok(())
    }

    unsafe fn destroy_swapchain_objects(&mut self, d: &ash::Device) {
        for v in self.views.drain(..) {
            d.destroy_image_view(v, None);
        }
        for s in self.done.drain(..) {
            d.destroy_semaphore(s, None);
        }
        self.images.clear();
    }

    /// Renders and presents one frame. Returns the GPU time of the previous frame's overlay pass,
    /// and whether the swapchain needs to be recreated.
    unsafe fn frame(
        &mut self,
        ctx: &Context,
        ov: &mut Overlay,
        dt: f32,
        build: impl FnOnce(&dear_imgui_rs::Ui),
    ) -> Res<(Option<f64>, bool)> {
        let d = &ctx.device;
        d.wait_for_fences(&[self.fence], true, u64::MAX)?;
        let mut gpu_ms = None;
        if self.queried && ctx.timestamp_valid_bits > 0 {
            let mut ts = [0u64; 2];
            if d.get_query_pool_results(self.queries, 0, &mut ts, vk::QueryResultFlags::TYPE_64)
                .is_ok()
            {
                gpu_ms = Some(ts[1].wrapping_sub(ts[0]) as f64 * f64::from(ctx.timestamp_period_ns) * 1e-6);
            }
        }
        ov.after_gpu_idle()?;
        if self.swapchain == vk::SwapchainKHR::null() {
            return Ok((gpu_ms, true));
        }
        let (index, suboptimal) =
            match self
                .swapchain_fn
                .acquire_next_image(self.swapchain, u64::MAX, self.acquire, vk::Fence::null())
            {
                Ok(r) => r,
                Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => return Ok((gpu_ms, true)),
                Err(e) => return Err(e.into()),
            };
        d.reset_fences(&[self.fence])?;
        let cb = self.cb;
        let image = self.images[index as usize];
        d.reset_command_buffer(cb, vk::CommandBufferResetFlags::empty())?;
        d.begin_command_buffer(
            cb,
            &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
        )?;
        d.cmd_reset_query_pool(cb, self.queries, 0, 2);
        barrier(
            d,
            cb,
            image,
            vk::ImageLayout::UNDEFINED,
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
            vk::AccessFlags2::NONE,
            vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
            vk::AccessFlags2::COLOR_ATTACHMENT_READ | vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
        );
        d.cmd_write_timestamp2(cb, vk::PipelineStageFlags2::ALL_COMMANDS, self.queries, 0);
        // A real build draws the CRT output first (encode pass) and the overlay with Load::Keep.
        ov.record(
            cb,
            self.views[index as usize],
            self.extent,
            Load::Clear(BACKGROUND),
            dt,
            build,
        )?;
        d.cmd_write_timestamp2(cb, vk::PipelineStageFlags2::ALL_COMMANDS, self.queries, 1);
        barrier(
            d,
            cb,
            image,
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            vk::ImageLayout::PRESENT_SRC_KHR,
            vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
            vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
            vk::PipelineStageFlags2::NONE,
            vk::AccessFlags2::NONE,
        );
        d.end_command_buffer(cb)?;
        let waits = [vk::SemaphoreSubmitInfo::default()
            .semaphore(self.acquire)
            .stage_mask(vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT)];
        let signals = [vk::SemaphoreSubmitInfo::default()
            .semaphore(self.done[index as usize])
            .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)];
        let cbs = [vk::CommandBufferSubmitInfo::default().command_buffer(cb)];
        let submit = [vk::SubmitInfo2::default()
            .wait_semaphore_infos(&waits)
            .signal_semaphore_infos(&signals)
            .command_buffer_infos(&cbs)];
        d.queue_submit2(ctx.queue, &submit, self.fence)?;
        self.queried = true;
        let wait = [self.done[index as usize]];
        let swapchains = [self.swapchain];
        let indices = [index];
        let present = self.swapchain_fn.queue_present(
            ctx.queue,
            &vk::PresentInfoKHR::default()
                .wait_semaphores(&wait)
                .swapchains(&swapchains)
                .image_indices(&indices),
        );
        let stale = match present {
            Ok(sub) => sub || suboptimal,
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => true,
            Err(e) => return Err(e.into()),
        };
        Ok((gpu_ms, stale))
    }

    unsafe fn destroy(&mut self, ctx: &Context) {
        let d = &ctx.device;
        let _ = d.device_wait_idle();
        self.destroy_swapchain_objects(d);
        self.swapchain_fn.destroy_swapchain(self.swapchain, None);
        self.surface_fn.destroy_surface(self.surface, None);
        d.destroy_semaphore(self.acquire, None);
        d.destroy_fence(self.fence, None);
        d.destroy_query_pool(self.queries, None);
        d.destroy_command_pool(self.pool, None);
    }
}

/// Counters for the self-test summary.
#[derive(Default)]
struct Stats {
    opens: u32,
    opens_ok: u32,
    closes: u32,
    closes_ok: u32,
    text_ok: u32,
    closes_to_source: u32,
    f11_toggles: u32,
    methods_open: Vec<&'static str>,
    methods_close: Vec<&'static str>,
    gpu_ms: Vec<f64>,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("live: {e}");
        std::process::exit(1);
    }
}

fn run() -> Res<()> {
    let args = parse_args()?;
    let log = Arc::new(Log {
        file: Mutex::new(File::create(&args.csv)?),
        start: Instant::now(),
        source_title: args.source_title.clone(),
    });
    if let Ok(mut f) = log.file.lock() {
        f.write_all(b"t_ms,kind,detail,fg_hwnd,fg_title,fg_is_self,fg_is_source,menu_open\n")?;
    }
    log.row(
        "start",
        &format!("fullscreen={} selftest={}", args.fullscreen, args.selftest),
    );

    // SAFETY: process-wide setting made before any window exists.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    // SAFETY: window class and window of this module; the window procedure is a plain function.
    let hwnd = unsafe {
        let hinstance = GetModuleHandleW(None)?;
        let class = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance.into(),
            lpszClassName: w!("ucrt_imgui_spike"),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        RegisterClassW(&class);
        CreateWindowExW(
            WINDOW_EX_STYLE(EX_BASE.0 | WS_EX_NOACTIVATE.0),
            w!("ucrt_imgui_spike"),
            w!("ultimatecrt imgui spike (T-08)"),
            WINDOWED_STYLE,
            100,
            100,
            960,
            600,
            None,
            None,
            Some(hinstance.into()),
            None,
        )?
    };
    SELF_HWND.store(hwnd.0 as isize, Ordering::SeqCst);
    if args.fullscreen {
        set_fullscreen(hwnd, true);
    }
    // SAFETY: our window; SW_SHOWNOACTIVATE does not take the foreground.
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }
    let mut fullscreen = args.fullscreen;
    log.row("window", &format!("shown hwnd={:#x}", hwnd.0 as isize));

    // SAFETY: hotkeys of our window, unregistered before it is destroyed.
    let (f11, f12) = unsafe {
        (
            RegisterHotKey(Some(hwnd), HOTKEY_F11, MOD_NOREPEAT, u32::from(VK_F11.0)),
            RegisterHotKey(Some(hwnd), HOTKEY_F12, MOD_NOREPEAT, u32::from(VK_F12.0)),
        )
    };
    log.row("hotkey", &format!("F11 {:?}", f11.as_ref().map_err(|e| e.message())));
    // Microsoft documents F12 as reserved for the debugger: registration may fail or the key may
    // break into an attached debugger. Recorded, not worked around.
    log.row("hotkey", &format!("F12 {:?}", f12.as_ref().map_err(|e| e.message())));

    // Foreground poll thread.
    let stop = Arc::new(AtomicBool::new(false));
    let poller = {
        let log = log.clone();
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut last = foreground().0;
            while !stop.load(Ordering::SeqCst) {
                let (fg, _) = foreground();
                if fg != last {
                    last = fg;
                    log.row("fg", "foreground changed");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        })
    };

    // Self-test driver: synthesises the hotkeys and typing, then closes the window.
    let driver = (args.selftest > 0).then(|| {
        let n = args.selftest;
        let log = log.clone();
        let target = hwnd.0 as isize;
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(2000));
            for _ in 0..2 {
                log.row("selftest", "send F11");
                tap(VK_F11);
                std::thread::sleep(Duration::from_millis(1500));
            }
            for i in 0..n {
                log.row("selftest", &format!("cycle {i}: send F12 (open)"));
                tap(VK_F12);
                std::thread::sleep(Duration::from_millis(1200));
                log.row("selftest", &format!("cycle {i}: type t08"));
                type_text("t08");
                std::thread::sleep(Duration::from_millis(500));
                log.row("selftest", &format!("cycle {i}: send F12 (close)"));
                tap(VK_F12);
                std::thread::sleep(Duration::from_millis(1500));
            }
            // SAFETY: posting to a window handle; harmless if the window is already gone.
            unsafe {
                let _ = PostMessageW(Some(hwnd_of(target)), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        })
    });

    // Vulkan and ImGui.
    let validation = args.validation && imgui_spike::gpu::validation_layer_available();
    // SAFETY: the window and module are live until after the presenter and context are destroyed.
    let (ctx, surface) = unsafe {
        let hinstance = GetModuleHandleW(None)?;
        Context::with_window(hwnd.0 as isize, hinstance.0 as isize, validation)?
    };
    let counter = ctx.validation_counter();
    // SAFETY: surface of `ctx`; the presenter and overlay are destroyed before `ctx` drops.
    let mut presenter = unsafe { Presenter::new(&ctx, surface, client_size(hwnd))? };
    let mut ov = unsafe { Overlay::new(&ctx, FORMAT, 1)? };
    // SAFETY: our live window.
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    let scale = input::dpi_scale(dpi.max(96));
    ov.imgui.style_mut().scale_all_sizes(scale);
    ov.imgui.style_mut().set_font_scale_dpi(scale);
    log.row("vulkan", &format!("{} validation={validation} dpi={dpi}", ctx.gpu_name));

    let mut menu = MenuState::default();
    let mut stats = Stats::default();
    let mut remembered: Option<HWND> = None;
    let mut last = Instant::now();
    let mut stale = false;
    let mut frames = 0u64;
    let result = (|| -> Res<()> {
        loop {
            // SAFETY: standard message pump of this thread.
            unsafe {
                let mut msg = MSG::default();
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            let q = QUEUE.with(|q| std::mem::take(&mut *q.borrow_mut()));
            if q.quit {
                return Ok(());
            }
            stale |= q.resized;
            for id in q.hotkeys {
                if id == HOTKEY_F11 {
                    fullscreen = !fullscreen;
                    set_fullscreen(hwnd, fullscreen);
                    stats.f11_toggles += 1;
                    stale = true;
                    log.row("f11", &format!("fullscreen={fullscreen}"));
                } else if id == HOTKEY_F12 {
                    if MENU_OPEN.load(Ordering::SeqCst) {
                        // Close: back to non-activating, foreground to the remembered window.
                        stats.closes += 1;
                        let typed = menu.name.contains("t08");
                        stats.text_ok += u32::from(typed);
                        menu.name.clear();
                        MENU_OPEN.store(false, Ordering::SeqCst);
                        set_ex_style(hwnd, true);
                        ov.feed(&[UiEvent::MousePos([-f32::MAX, -f32::MAX]), UiEvent::Focus(false)]);
                        let target = remembered.take();
                        let how = match target {
                            Some(t) => bring_to_front(t, args.method),
                            None => "no-remembered-window",
                        };
                        let (_, title) = foreground();
                        stats.closes_ok += u32::from(how != "failed" && target.is_some());
                        stats.closes_to_source += u32::from(log.is_source(&title));
                        stats.methods_close.push(how);
                        log.row("close", &format!("restore={how} typed_ok={typed}"));
                    } else {
                        // Open: remember the foreground, become activatable, take the foreground.
                        stats.opens += 1;
                        // SAFETY: plain query.
                        let prev = unsafe { GetForegroundWindow() };
                        remembered = (prev != hwnd && !prev.is_invalid()).then_some(prev);
                        MENU_OPEN.store(true, Ordering::SeqCst);
                        set_ex_style(hwnd, false);
                        let how = bring_to_front(hwnd, args.method);
                        stats.opens_ok += u32::from(how != "failed");
                        stats.methods_open.push(how);
                        menu.focus_text = true;
                        let mut ev = Vec::new();
                        CONV.with(|c| c.borrow_mut().set_modifiers(modifiers_now(), &mut ev));
                        ev.push(UiEvent::Focus(true));
                        ov.feed(&ev);
                        log.row("open", &format!("activate={how}"));
                    }
                }
            }
            if MENU_OPEN.load(Ordering::SeqCst) {
                ov.feed(&q.events);
            } else {
                // DPI changes still apply while the menu is closed.
                for e in &q.events {
                    if let UiEvent::DpiScale(s) = *e {
                        ov.imgui.style_mut().set_font_scale_dpi(s);
                    }
                }
            }
            if stale {
                // SAFETY: device idle before the swapchain is replaced.
                unsafe {
                    ctx.device.device_wait_idle()?;
                    presenter.recreate(&ctx, client_size(hwnd))?;
                }
                stale = false;
            }
            let now = Instant::now();
            let dt = (now - last).as_secs_f32();
            last = now;
            let open = MENU_OPEN.load(Ordering::SeqCst);
            let lines = [
                format!("frame {frames}  {:.1} ms", dt * 1e3),
                format!("menu {}  fullscreen {fullscreen}", if open { "open" } else { "closed" }),
                format!("overlay GPU {:.3} ms", stats.gpu_ms.last().copied().unwrap_or(0.0)),
            ];
            let extent = presenter.extent;
            // SAFETY: one frame in flight, waited on by `frame` before reuse.
            let (gpu, out_of_date) = unsafe {
                presenter.frame(&ctx, &mut ov, dt, |ui| {
                    overlay::stats_overlay(ui, &lines, [extent.width as f32 - 260.0, 10.0]);
                    if open {
                        overlay::settings_menu(ui, &mut menu);
                    }
                })?
            };
            if let Some(ms) = gpu {
                stats.gpu_ms.push(ms);
            }
            stale |= out_of_date;
            frames += 1;
        }
    })();

    stop.store(true, Ordering::SeqCst);
    let _ = poller.join();
    if let Some(d) = driver {
        let _ = d.join();
    }
    let _ = ov.destroy();
    // SAFETY: device idle inside `destroy`; the context drops after.
    unsafe {
        presenter.destroy(&ctx);
    }
    drop(ov);
    drop(ctx);
    // SAFETY: our window and hotkeys.
    unsafe {
        let _ = UnregisterHotKey(Some(hwnd), HOTKEY_F11);
        let _ = UnregisterHotKey(Some(hwnd), HOTKEY_F12);
        let _ = DestroyWindow(hwnd);
    }

    let mut ms = stats.gpu_ms.clone();
    ms.sort_by(f64::total_cmp);
    let median = ms.get(ms.len() / 2).copied().unwrap_or(0.0);
    let summary = format!(
        "selftest_cycles={}\nframes={frames}\nf11_registered={}\nf12_registered={}\nf11_toggles={}\nopens={}\nopens_ok={}\ncloses={}\ncloses_ok={}\ncloses_to_source={}\ntext_ok={}\nmethods_open={}\nmethods_close={}\noverlay_gpu_ms_median={median:.4}\nvalidation={validation}\nvalidation_messages={}\nerror={}\n",
        args.selftest,
        f11.is_ok(),
        f12.is_ok(),
        stats.f11_toggles,
        stats.opens,
        stats.opens_ok,
        stats.closes,
        stats.closes_ok,
        stats.closes_to_source,
        stats.text_ok,
        stats.methods_open.join(" "),
        stats.methods_close.join(" "),
        counter.count(),
        result.as_ref().err().map(|e| e.to_string()).unwrap_or_default(),
    );
    std::fs::write(&args.summary, &summary)?;
    print!("{summary}");
    result
}
