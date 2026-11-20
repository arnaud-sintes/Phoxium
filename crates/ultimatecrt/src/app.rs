//! The application: window lookup, capture, output window and the render loop.

use std::time::{Duration, Instant};

use windows::Win32::{
    System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED},
    UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2},
};

use crate::{
    adapters, capture, cli, diag, gpu, hotkeys, lifecycle, live, modes, output, placement, presets, scaling, settings,
    source, sources, Res,
};

/// The rectangle the output window will cover, as far as it is known before the window exists (HW-3): in
/// fullscreen the `--monitor` or the monitor of the source window `follow`; windowed, the remembered rectangle
/// when it is still on a monitor (else Windows places the window: unknown).
fn predicted_cover(
    fullscreen: bool,
    monitor: Option<usize>,
    follow: Option<windows::Win32::Foundation::HWND>,
    saved: Option<[i32; 4]>,
) -> Option<adapters::Rect> {
    let monitors = output::monitor_irects();
    let r = if fullscreen {
        match monitor {
            Some(i) => monitors.get(i).copied(),
            None => follow.map(output::monitor_of),
        }
    } else {
        saved
            .map(modes::IRect::from_array)
            .filter(|s| modes::usable_rect(s, &monitors))
    };
    r.map(|r| (r.left, r.top, r.right, r.bottom))
}

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

    if args.list_adapters {
        let stored = settings::default_dir()
            .map(|d| settings::load(&d).settings)
            .unwrap_or_default();
        let covered = predicted_cover(args.fullscreen, args.monitor, None, stored.global.output.windowed);
        return adapters::run(args.gpu, &stored.global.gpu.adapter, covered);
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
    // T-50: --preset (else the settings preset), then the settings overrides; an unknown preset stops here.
    let mut params = crate::render_cmd::live_params(args.preset.as_deref())?;
    let synthetic = !matches!(kind, source::Source::Capture);
    let mut synthetic_n = 0u64;
    // T-96: the source profile of the target (executable and class) applies on top of the preset and the
    // global overrides, the command-line options on top of the profile.
    let mut profile = settings::SourceProfile::default();
    let found = if synthetic {
        None
    } else {
        let needle = args.window.clone().unwrap_or_else(|| "pixel-demo".into());
        let source = sources::find_by_title(&needle)
            .ok_or_else(|| format!("no visible window with \"{needle}\" in its title (try --list-windows)"))?;
        println!(
            "source: \"{}\" ({}x{}, class {})",
            source.title, source.width, source.height, source.class
        );
        // Follows the target across closes and relaunches (T-56): same executable and class.
        let target = lifecycle::TargetKey::of(&sources::WindowEntry::from_info(&source));
        if let (Some((exe, class)), Some(dir)) = (target.profile_key(), settings::default_dir()) {
            if let Some(p) = settings::load(&dir).settings.find_source(exe, class) {
                println!("source profile: {exe} ({class})");
                profile = p.clone();
                for w in profile.apply_to(&mut params) {
                    eprintln!("warning: {w}");
                }
            }
        }
        Some((source, target))
    };
    args.apply_source(&mut params)?;
    let (map, warnings) = scaling::SourceMap::resolve(
        &params,
        args.crop.or(profile.crop_rect()),
        args.divisor.or(profile.divisor_pair()),
    );
    for w in &warnings {
        eprintln!("warning: {w}");
    }
    // T-54, T-57, HW-3: the hotkey chords, the remembered window rectangle and the GPU choice come from the
    // settings file.
    let settings_dir = settings::default_dir();
    let stored = settings_dir
        .as_deref()
        .map(|d| settings::load(d).settings)
        .unwrap_or_default();
    // HW-3: the adapter that runs the chain. A first guess from where the output will go starts the capture; the
    // real choice is made once the output window exists (below), and the capture follows it if it differs.
    let topology = adapters::Topology::read()?;
    let gpu_spec = adapters::AdapterSpec::parse(&stored.global.gpu.adapter);
    let guess = topology.select(
        args.gpu,
        &gpu_spec,
        predicted_cover(
            args.fullscreen,
            args.monitor,
            found.as_ref().map(|(s, _)| s.hwnd),
            stored.global.output.windowed,
        ),
    );
    let (mut capture, follow, mut reattach) = match found {
        None => (None, windows::Win32::Foundation::HWND::default(), None),
        Some((source, target)) => {
            let capture = capture::Capture::start(source.hwnd, map.client_area, guess.capture_luid())?;
            println!("capture: Windows.Graphics.Capture on {} (CPU copy)", capture.adapter);
            let reattach = lifecycle::Reattacher::new(target, source.hwnd.0 as isize);
            (Some(capture), source.hwnd, Some(reattach))
        }
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
    // Initial windowed size (D-18: no assumed host resolution): the largest whole multiple of the frame
    // within 5/6 of the height and the width of the chosen (else the first) monitor.
    let monitors = output::monitor_rects();
    let screen = monitors
        .get(args.monitor.unwrap_or(0))
        .or(monitors.first())
        .map_or((1280, 720), |r| (r.right - r.left, r.bottom - r.top));
    let fit = |avail: i32, size: u32| (avail.max(1) as u32 * 5 / 6) / size.max(1);
    let scale = fit(screen.1, frame.height).min(fit(screen.0, frame.width)).max(1) as i32;
    let (bindings, hotkey_warnings) = hotkeys::Bindings::from_settings(&stored.global.hotkeys);
    for w in &hotkey_warnings {
        eprintln!("warning: {w}");
    }
    let win = output::create(&output::Options {
        fullscreen: args.fullscreen,
        monitor: args.monitor,
        follow,
        client_size: (frame.width as i32 * scale, frame.height as i32 * scale),
        hotkeys: bindings,
        saved_rect: stored.global.output.windowed.map(modes::IRect::from_array),
    })?;
    // HW-3: the adapter owning the monitor the output window really covers (auto), or the requested one.
    let cover = output::outer_rect(win.hwnd);
    let mut selection = topology.select(
        args.gpu,
        &gpu_spec,
        Some((cover.left, cover.top, cover.right, cover.bottom)),
    );
    let mut renderer = gpu::Renderer::new(
        win.hwnd.0 as isize,
        win.hinstance,
        selection.device_request(),
        args.validation,
        args.integer,
    )?;
    let got = &renderer.context().gpu;
    selection.note_renderer(adapters::VulkanDevice {
        index: got.index,
        name: got.name.clone(),
        luid: got.luid.map(adapters::Luid::from_vulkan),
    });
    // The capture device on the renderer's adapter: restart the session when the first guess differs.
    let mut captured_base = 0u64; // frames of the sessions already closed
    let wanted = selection.capture_luid();
    if let Some(old) = capture.take_if(|c| wanted.is_some_and(|l| l != c.adapter_luid)) {
        captured_base += old.frames_captured();
        drop(old); // the old session closes before the new one starts
        let c = capture::Capture::start(follow, map.client_area, wanted)?;
        println!("capture: restarted on {} (the renderer's adapter)", c.adapter);
        frame.seq = 0; // the new session counts from 1
        capture = Some(c);
    }
    if let Some(c) = &capture {
        selection.note_capture(&c.adapter, c.adapter_luid);
    }
    renderer.set_params(params);
    println!("render: Vulkan on {}", renderer.gpu_name);
    println!("{}", selection.summary());
    for w in &selection.warnings {
        eprintln!("warning: gpu: {w}");
    }
    let capture_luid = selection.capture_luid();
    adapters::publish(adapters::Current {
        selection,
        adapters: topology.entries(),
        setting: stored.global.gpu.adapter.clone(),
    });
    // The output never takes the keyboard (OUT-2), so Esc cannot reach it; only the quit hotkey works.
    println!("quit: Ctrl+Alt+Q");
    println!("hotkeys: {}", win.hotkeys.summary());

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
    let mut notice: Option<String> = None;
    // Lost source (T-56): reminders, then a fullscreen output drops to a window, or the app exits.
    let mut lost_watch = lifecycle::LostWatch::new(args.lost, args.fullscreen);
    let windowed_size = (frame.width as i32 * scale, frame.height as i32 * scale);
    let mut readout_key = ((0u32, 0u32), (0u32, 0u32));
    // Hotkey actions and output modes (T-54, T-57). `live` owns the one settings instance of the session.
    let library = presets::Library::load(settings_dir.as_deref().map(presets::user_dir).as_deref());
    let current_preset = args
        .preset
        .as_deref()
        .or(Some(stored.global.preset.as_str()).filter(|p| !p.is_empty()))
        .and_then(|id| library.find(id))
        .map(|p| p.id.clone());
    // T-53: what the menu compares against and resets to: the preset in force (else the table defaults) with
    // the source profile and the command-line options on top, as at start but without the settings overrides.
    let baseline_of = |library: &presets::Library, preset: Option<&str>| -> crate::params::Params {
        let mut p = preset
            .and_then(|id| library.find(id))
            .map(presets::Preset::params)
            .unwrap_or_default();
        let _ = profile.apply_to(&mut p);
        if let Err(e) = args.apply_source(&mut p) {
            eprintln!("warning: {e}");
        }
        p
    };
    let mut baseline = baseline_of(&library, current_preset.as_deref());
    let ui_settings_dir = settings_dir.clone();
    // T-43: the burn-in wear map of the preset in force, loaded while burn-in is on, saved periodically and at exit.
    let mut tube = crate::tube_state::TubeState::new(settings_dir.clone(), args.reset_burnin, Instant::now());
    let mut live = live::Live::new(
        args.fullscreen,
        stored,
        settings_dir,
        library,
        current_preset,
        output::default_outer_size(windowed_size),
    );
    // T-52a: the settings menu and the stats overlay, drawn over the output, reached by the hotkeys through
    // `controls::UiHook`. Created after the renderer's device, dropped before it (declared after it, and
    // dropped explicitly at the end of `run`). Its layout, menu key and presets come from `live`.
    let mut ui = crate::ui::Ui::new(
        renderer.context(),
        renderer.output_format(),
        crate::ui::UiConfig::live(
            win.hwnd.0 as isize,
            follow.0 as isize,
            live.settings(),
            ui_settings_dir,
            live.library().clone(),
            &win.hotkeys,
        ),
    )?;
    let mut ui_state = crate::ui::UiState::default();
    let mut stepped_preset = live.current_preset().map(str::to_string);
    loop {
        if !output::pump() {
            break;
        }
        live.tick(&mut ui, win.hwnd, &mut renderer, &mut params, &mut lost_watch);
        if live.controls.quit {
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
                    match capture::Capture::start(h, map.client_area, capture_luid) {
                        Ok(c) => {
                            capture = Some(c);
                            r.attach_result(now, hwnd, true);
                            ui.set_source(Some(hwnd));
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
            match lost_watch.update(now, r.is_attached()) {
                lifecycle::LostAction::None => {}
                lifecycle::LostAction::Remind => println!("{}", lifecycle::LOST_REMINDER),
                lifecycle::LostAction::GoWindowed => {
                    output::drop_to_windowed(win.hwnd, windowed_size);
                    live.dropped_to_windowed();
                    println!(
                        "source lost for {:.0} s: the output is now a normal window (Esc closes it)",
                        args.lost.wait_s
                    );
                }
                lifecycle::LostAction::Exit => {
                    println!("source lost: exiting (--exit-on-lost)");
                    break;
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
            let region = map.crop.map_or(full, |c| c.clamp(frame.width, frame.height));
            let key = (frame.width, frame.height, map.crop);
            if key != detect_key {
                detect_key = key;
                detector.reset();
            }
            let div = match map.divisor.or(synthetic.then_some((1, 1))) {
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
            // That alone also holds for text at 1:1, so the watch's verdict needs filtered edges on a
            // regular grid (an upscale of a low-resolution source), see `lifecycle::EdgeStats`.
            let eligible = capture.is_some() && map.divisor.is_none() && detector.is_locked() && div == (1, 1);
            if smoothing.feed(&frame.data, frame.width, region, eligible) {
                println!("warning: {}", lifecycle::SMOOTHED_WARNING);
            }
        }
        let upload = fresh.then_some((frame.width, frame.height, frame.data.as_slice()));
        // SRC-8: a source size change or a new capture session clears the persistence history (T-41).
        if size_watch.take_history_clear() {
            renderer.clear_history();
        }
        let out_size = output::client_size(win.hwnd);
        // T-52a: the menu's input, what the menu and the overlay show, and what the user changed.
        for (m, w, l) in output::take_ui_messages() {
            ui.handle_win32_message(m, w, l);
        }
        // A preset hotkey stepped to another preset: it is the new reference of the menu.
        if live.current_preset() != stepped_preset.as_deref() {
            stepped_preset = live.current_preset().map(str::to_string);
            baseline = baseline_of(live.library(), stepped_preset.as_deref());
        }
        if ui.is_open() || ui.overlay_on() {
            ui_state.params = params;
            // The preset in force, also when a preset hotkey stepped to it.
            ui_state.preset = live.current_preset().map(str::to_string);
            ui_state.baseline = Some(baseline);
            ui_state.headroom = renderer.headroom();
            ui_state.placement = Some(placement::readout(&params, view.native, out_size, args.integer));
        }
        // T-53: what the menu changed: the library (a preset saved or reloaded), the preset, the parameters
        // (applied live, recorded in the shared settings, written debounced), the adapter for the next start.
        let changes = ui.frame(&ui_state);
        if let Some(lib) = changes.library {
            live.set_library(lib);
        }
        if let Some(mut p) = changes.params {
            if changes.preset.is_some() {
                // A new preset: the source profile and the command-line options apply on top again.
                for w in profile.apply_to(&mut p) {
                    eprintln!("warning: {w}");
                }
                args.apply_source(&mut p)?;
                // The preset hotkeys step on from the preset chosen in the menu.
                live.set_current_preset(changes.preset.clone());
                live.note_preset_selected(changes.preset.as_deref());
                stepped_preset = changes.preset.clone();
                baseline = baseline_of(live.library(), changes.preset.as_deref());
                // A saved user preset keeps the edits made before the save: recorded against the new preset.
                live.note_params_edit(&baseline, &p, &baseline);
                ui_state.preset = changes.preset;
            } else {
                live.note_params_edit(&params, &p, &baseline);
            }
            params = p;
            renderer.set_params(params);
        }
        if let Some(adapter) = changes.gpu_adapter {
            println!("gpu: adapter for the next start set to \"{adapter}\" (restart to apply)");
            live.set_gpu_adapter(adapter);
        }
        // T-43: burn-in map of the preset in force (load, periodic save off the render thread), menu actions.
        tube.tick(&mut renderer, &params, live.current_preset(), Instant::now());
        match changes.wear_action {
            Some(crate::ui::WearAction::BurnInNow(h)) if params.burnin_enable => tube.burn_in_now(&mut renderer, h),
            Some(crate::ui::WearAction::NewTube) if params.burnin_enable => tube.reset(&mut renderer),
            _ => {}
        }
        for m in tube.take_messages() {
            println!("{m}");
        }
        // The settings the menu changed: once the edits pause, at once when the menu is closed.
        live.flush_settings(!ui.is_open());
        // The menu layout, once the menu closed after a change: into the shared settings and the file.
        if let Some(layout) = ui.take_layout_change() {
            live.set_ui_layout(layout);
        }
        renderer.render_with_ui(upload, view, out_size, Some(&mut ui))?;
        fresh = false;
        // T-96: the resolved placement, printed with --stats when the native raster or the output changes.
        if args.stats && (view.native, out_size) != readout_key {
            readout_key = (view.native, out_size);
            println!("{}", placement::readout(&params, view.native, out_size, args.integer));
        }
        renders += 1;
        if let Some(l) = log.as_mut() {
            l.tick(captured_base + capture.as_ref().map_or(0, |c| c.frames_captured()));
        }

        if stat_time.elapsed() >= Duration::from_secs(1) {
            let handbacks = output::take_handbacks();
            if handbacks > 0 {
                println!(
                    "focus: Windows activated the output {handbacks}x at {now:.1} s; foreground handed back (T-07)"
                );
            }
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
                // T-32: the output refresh against the field rate (interlace on, fields alternating).
                let secs = stat_time.elapsed().as_secs_f64();
                crate::interlace::notice(&params, (captured - last_captured) as f64 / secs, renders as f64 / secs)
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
            (ui_state.timings, ui_state.capture_fps, ui_state.render_fps) = (
                gpu.clone(),
                (captured - last_captured) as f64 / secs,
                renders as f64 / secs,
            );
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
    // A menu still open at exit gives the foreground back and its layout is saved with the window rectangle.
    ui.close_menu();
    if let Some(layout) = ui.take_layout_change() {
        live.set_ui_layout(layout);
    }
    live.finish(win.hwnd);
    // T-43: the wear map read back once and written (when burn-in loaded one in this session).
    tube.finish(&mut renderer);
    for m in tube.take_messages() {
        println!("{m}");
    }
    if let Some(l) = log.as_mut() {
        l.summary();
    }
    // The UI first: its ImGui renderer lives on the renderer's device (T-52a).
    drop(ui);
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
