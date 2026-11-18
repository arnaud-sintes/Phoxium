//! The application: window lookup, capture, output window and the render loop.

use std::time::{Duration, Instant};

use windows::Win32::{
    System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED},
    UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2},
};

use crate::{capture, cli, diag, gpu, lifecycle, output, scaling, source, sources, Res};

const NO_FRAME: &str =
    "no frame captured within 5 seconds (the window must be visible and redrawing: try moving or resizing it)";

pub fn run() -> Res<()> {
    // `render` is an offline sub-command: no window, no capture, no COM setup.
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.first().map(String::as_str) == Some("render") {
        return crate::render_cmd::run(&argv[1..]);
    }
    let args = cli::parse_args()?;
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        RoInitialize(RO_INIT_MULTITHREADED)?;
    }

    if args.list_gpus {
        for g in gpu::list_gpus()? {
            println!(
                "[{}] {} (vendor 0x{:04X}, {:?}, Vulkan {}.{})",
                g.index,
                g.name,
                g.vendor_id,
                g.kind,
                ash::vk::api_version_major(g.api_version),
                ash::vk::api_version_minor(g.api_version)
            );
        }
        return Ok(());
    }
    if args.list_windows {
        for w in sources::list_windows() {
            println!(
                "{:>10}  {:>5}x{:<5} {:<28} {}",
                w.hwnd.0 as usize, w.width, w.height, w.class, w.title
            );
        }
        for (i, r) in output::monitor_rects().iter().enumerate() {
            println!(
                "monitor {i}: {}x{} at ({}, {})",
                r.right - r.left,
                r.bottom - r.top,
                r.left,
                r.top
            );
        }
        return Ok(());
    }

    let kind = source::Source::choose(args.pattern.as_deref(), args.pattern_size, args.image.as_deref())?;
    let synthetic = !matches!(kind, source::Source::Capture);
    let mut synthetic_n = 0u64;
    let (mut capture, follow, mut reattach) = if synthetic {
        (None, windows::Win32::Foundation::HWND::default(), None)
    } else {
        let needle = args.window.clone().unwrap_or_else(|| "pixel-demo".into());
        let source = sources::find_by_title(&needle)
            .ok_or_else(|| format!("no visible window with \"{needle}\" in its title (try --list-windows)"))?;
        println!(
            "source: \"{}\" ({}x{}, class {})",
            source.title, source.width, source.height, source.class
        );
        let capture = capture::Capture::start(source.hwnd, !args.no_crop)?;
        println!("capture: Windows.Graphics.Capture on {} (CPU copy)", capture.adapter);
        // Follows the target across closes and relaunches (T-56): same executable and class.
        let target = lifecycle::TargetKey::of(&sources::WindowEntry::from_info(&source));
        let reattach = lifecycle::Reattacher::new(target, source.hwnd.0 as isize);
        (Some(capture), source.hwnd, Some(reattach))
    };
    // Next frame of the source: the capture, or a pattern or image (static ones only once).
    let mut poll = |capture: Option<&capture::Capture>, frame: &mut capture::Frame| -> Res<bool> {
        match capture {
            Some(c) => Ok(c.take_frame(frame)),
            None if !synthetic => Ok(false), // target lost, waiting for a relaunch
            None if synthetic_n == 0 || kind.is_animated() => {
                if let Some(f) = kind.frame(synthetic_n)? {
                    *frame = f;
                }
                synthetic_n += 1;
                Ok(true)
            }
            None => Ok(false),
        }
    };
    let mut frame = capture::Frame::default();

    if let Some(path) = &args.save_capture {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !poll(capture.as_ref(), &mut frame)? {
            if Instant::now() > deadline {
                return Err(NO_FRAME.into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        save_png(path, &frame)?;
        println!("saved {}x{} capture to {path}", frame.width, frame.height);
        return Ok(());
    }

    // Wait for the first frame to size the window.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !poll(capture.as_ref(), &mut frame)? {
        if Instant::now() > deadline {
            return Err(NO_FRAME.into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let scale = (900 / frame.height.max(1)).max(1) as i32;
    let win = output::create(&output::Options {
        fullscreen: args.fullscreen,
        no_activate: args.no_activate,
        monitor: args.monitor,
        follow,
        client_size: (frame.width as i32 * scale, frame.height as i32 * scale),
    })?;
    let mut renderer = gpu::Renderer::new(
        win.hwnd.0 as isize,
        win.hinstance,
        args.gpu,
        args.validation,
        args.integer,
    )?;
    println!("render: Vulkan on {}", renderer.gpu_name);
    println!(
        "quit: Ctrl+Alt+Q{}",
        if args.fullscreen || args.no_activate {
            ""
        } else {
            " or Esc in the output window"
        }
    );

    let mut log = args
        .log
        .as_deref()
        .map(|p| diag::Log::create(p, follow, renderer.timing_names()))
        .transpose()?;
    if args.stats {
        if let Some(why) = renderer.timing_unavailable() {
            println!("gpu timings unavailable: {why}");
        }
    }
    let start = Instant::now();
    let mut stat_time = Instant::now();
    let (mut renders, mut last_captured) = (0u32, 0u64);
    let mut fresh = true; // upload the first frame
    let mut detector = scaling::Detector::default();
    let mut detect_key = (0u32, 0u32, None);
    let mut view = scaling::View::full(frame.width, frame.height);
    // Target lifecycle (T-56): reattach, black or stale frames, smoothed source, size changes.
    let mut health = lifecycle::Health::new(0.0);
    let mut smoothing = lifecycle::SmoothingWatch::default();
    let mut size_watch = lifecycle::SizeWatch::default();
    let mut captured_base = 0u64; // frames of the sessions already closed
    let mut notice: Option<String> = None;
    loop {
        if !output::pump() {
            break;
        }
        if args.exit_after.is_some_and(|s| start.elapsed().as_secs_f64() > s) {
            break;
        }
        let now = start.elapsed().as_secs_f64();
        if let Some(r) = reattach.as_mut() {
            let closed = capture.as_ref().is_some_and(|c| c.is_closed());
            match r.update(now, closed, sources::window_exists, sources::list_entries) {
                lifecycle::Step::Idle => {}
                lifecycle::Step::Lost => {
                    captured_base += capture.as_ref().map_or(0, |c| c.frames_captured());
                    capture = None;
                    println!("source lost: waiting for {}", r.target().describe());
                }
                lifecycle::Step::TryAttach(hwnd) => {
                    let h = windows::Win32::Foundation::HWND(hwnd as *mut std::ffi::c_void);
                    match capture::Capture::start(h, !args.no_crop) {
                        Ok(c) => {
                            capture = Some(c);
                            r.attach_result(now, hwnd, true);
                            frame.seq = 0; // the new session counts from 1
                            health.reset(now);
                            size_watch.request_history_clear();
                            println!("source reattached: {}", r.target().describe());
                        }
                        Err(e) => {
                            r.attach_result(now, hwnd, false);
                            if r.failures() == 1 {
                                println!("reattach failed, retrying: {e}");
                            }
                        }
                    }
                }
            }
        }
        if poll(capture.as_ref(), &mut frame)? {
            if size_watch.observe(frame.width, frame.height) {
                // SRC-8: the renderer rebuilds its source-sized resources when it gets this frame
                // and the detector restarts below; the history is cleared by the persistence stage
                // (T-41) through `size_watch.take_history_clear()`.
                smoothing.reset();
                println!("source size changed: {}x{}", frame.width, frame.height);
            }
            if capture.is_some() {
                let black = health.wants_sample(now).then(|| lifecycle::is_black(&frame.data));
                health.note_frame(now, black);
            }
            fresh = true;
            let full = scaling::Rect {
                x: 0,
                y: 0,
                w: frame.width,
                h: frame.height,
            };
            let region = args.crop.map_or(full, |c| c.clamp(frame.width, frame.height));
            let key = (frame.width, frame.height, args.crop);
            if key != detect_key {
                detect_key = key;
                detector.reset();
            }
            let div = match args.divisor.or(synthetic.then_some((1, 1))) {
                Some(d) => d,
                None => {
                    let was_locked = detector.is_locked();
                    detector.feed(&frame.data, frame.width, region);
                    if detector.is_locked() && !was_locked {
                        let d = detector.divisor();
                        if d == (1, 1) {
                            println!("source scaling: native (x1), {}x{}", region.w, region.h);
                        } else {
                            println!(
                                "source scaling: detected x{}/x{}, native {}x{}",
                                d.0,
                                d.1,
                                region.w / d.0,
                                region.h / d.1
                            );
                        }
                    }
                    detector.divisor()
                }
            };
            view = scaling::View::new(region, div);
            // SRC-2: a detected (not given) divisor of 1 on a locked detection means no integer scale.
            let eligible = capture.is_some() && args.divisor.is_none() && detector.is_locked() && div == (1, 1);
            if smoothing.feed(&frame.data, frame.width, region, eligible) {
                println!("warning: {}", lifecycle::SMOOTHED_WARNING);
            }
        }
        let upload = fresh.then_some((frame.width, frame.height, frame.data.as_slice()));
        // SRC-8: a source size change or a new capture session clears the persistence history (T-41).
        if size_watch.take_history_clear() {
            renderer.clear_history();
        }
        renderer.render(upload, view, output::client_size(win.hwnd))?;
        fresh = false;
        renders += 1;
        if let Some(l) = log.as_mut() {
            l.tick(captured_base + capture.as_ref().map_or(0, |c| c.frames_captured()));
        }

        if stat_time.elapsed() >= Duration::from_secs(1) {
            let captured = captured_base + capture.as_ref().map_or(0, |c| c.frames_captured());
            // The one message to show: target lost, black or silent frames, smoothed source.
            let now_notice = if reattach.as_ref().is_some_and(|r| !r.is_attached()) {
                reattach
                    .as_ref()
                    .map(|r| format!("waiting for {}", r.target().describe()))
            } else if let Some(m) = health.status(now).message() {
                Some(m)
            } else if smoothing.warning() {
                Some("smoothed source?".to_string())
            } else {
                None
            };
            if now_notice != notice {
                if let Some(m) = &now_notice {
                    println!("notice: {m}");
                }
                notice = now_notice;
            }
            let secs = stat_time.elapsed().as_secs_f64();
            // Mean GPU time per stage over this second (frames lag the CPU by at most the frames in flight).
            let gpu = renderer.take_timing_average();
            if let Some(l) = log.as_mut() {
                l.second(captured - last_captured, renders, secs, gpu.as_ref())?;
            }
            output::set_title(
                win.hwnd,
                &format!(
                    "ultimatecrt | capture {:.0} fps | render {:.0} fps | source {}x{} (div {}x{}) | {}{}",
                    (captured - last_captured) as f64 / secs,
                    renders as f64 / secs,
                    view.native.0,
                    view.native.1,
                    view.div.0,
                    view.div.1,
                    renderer.gpu_name,
                    notice.as_ref().map(|m| format!(" | {m}")).unwrap_or_default()
                ),
            );
            if args.stats {
                println!(
                    "capture {:.0} fps | render {:.0} fps | frame {}x{} | native {}x{} (div {}x{})",
                    (captured - last_captured) as f64 / secs,
                    renders as f64 / secs,
                    frame.width,
                    frame.height,
                    view.native.0,
                    view.native.1,
                    view.div.0,
                    view.div.1
                );
                if let Some(g) = &gpu {
                    println!("  {}", g.stats_line());
                }
            }
            stat_time = Instant::now();
            renders = 0;
            last_captured = captured;
        }
    }
    if let Some(l) = log.as_mut() {
        l.summary();
    }
    drop(renderer);
    drop(capture);
    Ok(())
}

fn save_png(path: &str, frame: &capture::Frame) -> Res<()> {
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut enc = png::Encoder::new(file, frame.width, frame.height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header()?;
    let mut rgba = Vec::with_capacity(frame.data.len());
    for px in frame.data.as_chunks::<4>().0 {
        rgba.extend_from_slice(&[px[2], px[1], px[0], 255]);
    }
    writer.write_image_data(&rgba)?;
    Ok(())
}
