//! The application: source selection, capture, output window and the render loop.
//!
//! The product needs no command line (A23): started without options, the output window opens with the window
//! picker ("Choose a window to capture", `ui::picker`) and the user attaches to a window there. The source
//! state machine is `picker::Phase` (NoSource -> Picking -> Attached -> Lost -> NoSource). `--window`, a
//! pattern or an image start with a source at once (developer and diagnostic paths, unchanged).

use std::time::{Duration, Instant};

use windows::Win32::{
    Foundation::HWND,
    System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED},
    UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2},
};

use crate::{
    adapters, bars, capture, cli, diag, gpu, hotkeys, lifecycle, live, modes, output,
    picker::{self, Event, Phase},
    placement, presets, scaling, settings, source, sources,
    ui::{AttachedView, SourceAction, SourcePrefs, SourceView},
    Res,
};

/// The rectangle the output window will cover, as far as it is known before the window exists (HW-3): in
/// fullscreen the `--monitor` or the monitor of the source window `follow`; windowed, the remembered rectangle
/// when it is still on a monitor (else Windows places the window: unknown).
fn predicted_cover(
    fullscreen: bool,
    monitor: Option<usize>,
    follow: Option<HWND>,
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

/// The frame shown without a source (behind the picker): black, the tube's own size scale.
const IDLE_SIZE: (u32, u32) = (320, 240);

fn idle_frame() -> capture::Frame {
    capture::Frame {
        width: IDLE_SIZE.0,
        height: IDLE_SIZE.1,
        data: [0u8, 0, 0, 255].repeat((IDLE_SIZE.0 * IDLE_SIZE.1) as usize),
        seq: 0,
    }
}

fn hwnd_of(h: isize) -> HWND {
    HWND(h as *mut std::ffi::c_void)
}

/// The window list entry (title, executable, class, size) of a window: from the picker's rows when it is
/// listed there, else queried.
fn entry_of(h: isize, rows: &[picker::Row]) -> sources::WindowEntry {
    if let Some(r) = rows.iter().find(|r| r.hwnd == h) {
        return sources::WindowEntry {
            hwnd: h,
            title: r.title.clone(),
            exe: r.exe.clone(),
            class: r.class.clone(),
            width: r.width,
            height: r.height,
        };
    }
    sources::list_entries()
        .into_iter()
        .find(|e| e.hwnd == h)
        .unwrap_or_else(|| sources::WindowEntry {
            hwnd: h,
            title: String::new(),
            exe: sources::executable_name(hwnd_of(h)),
            class: String::new(),
            width: 0,
            height: 0,
        })
}

/// The source profile of a window (`[[source]]` by executable and class), if any.
fn profile_for(settings: &settings::Settings, target: &lifecycle::TargetKey) -> Option<settings::SourceProfile> {
    let (exe, class) = target.profile_key()?;
    settings.find_source(exe, class).cloned()
}

/// The picker's rows now: candidates of the enumeration, recent windows first.
fn picker_rows(recent: &picker::Recent) -> Vec<picker::Row> {
    let mut rows = picker::candidates(&sources::enumerate());
    picker::sort_rows(&mut rows, recent.as_slice());
    rows
}

/// The window title: the program attached and the rates, or the invitation to choose one.
fn window_title(name: Option<&str>, rates: Option<String>) -> String {
    match (name, rates) {
        (Some(n), Some(r)) => format!("ultimateCRT | {n} | {r}"),
        (Some(n), None) => format!("ultimateCRT | {n}"),
        (None, Some(r)) => format!("ultimateCRT | {r}"),
        (None, None) => "ultimateCRT | choose a window to capture".to_string(),
    }
}

/// The program name of a target for the title: `pixel-demo.exe - pixel-demo`.
fn target_name(t: &lifecycle::TargetKey) -> String {
    let exe = t.exe.as_deref().unwrap_or("unknown program");
    if t.title.is_empty() {
        exe.to_string()
    } else {
        format!("{exe} - {}", t.title)
    }
}

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
    // A23: the parameters without any source profile; a profile is swapped on every attach on top of the preset
    // in force (`SourceProfile::switch_to`).
    let start_base = params;
    let mut synthetic = !matches!(kind, source::Source::Capture);
    let mut synthetic_n = 0u64;
    // T-54, T-57, HW-3, A23: the hotkey chords, the remembered window rectangle, the GPU choice and the source
    // selection come from the settings file.
    let settings_dir = settings::default_dir();
    let stored = settings_dir
        .as_deref()
        .map(|d| settings::load(d).settings)
        .unwrap_or_default();
    // A23: the windows recently in the foreground come first in the picker (the z-order seeds the list).
    let mut recent = picker::Recent::default();
    let mut start_message: Option<(bool, String)> = None;
    // The source at start: --window (developer path, an error when not found), else the last program when
    // `ask_at_start` is off and it runs, else none (the picker).
    let initial: Option<sources::WindowEntry> = if synthetic {
        None
    } else if let Some(needle) = args.window.as_deref() {
        let source = sources::find_by_title(needle)
            .ok_or_else(|| format!("no visible window with \"{needle}\" in its title (try --list-windows)"))?;
        Some(sources::WindowEntry::from_info(&source))
    } else {
        let rows = picker_rows(&recent);
        recent.seed(rows.iter().take(5).map(|r| r.hwnd));
        match picker::start_choice(
            stored.global.source.ask_at_start,
            stored.global.source.last.as_ref(),
            &rows,
        ) {
            picker::StartChoice::Attach(h) => Some(entry_of(h, &rows)),
            picker::StartChoice::Picker => None,
        }
    };
    if args.save_capture.is_some() && initial.is_none() && !synthetic {
        return Err("--save-capture needs --window (or --pattern, --image)".into());
    }
    // T-96: the source profile of the target (executable and class) applies on top of the preset and the
    // global overrides, the command-line options on top of the profile.
    let mut profile = settings::SourceProfile::default();
    let mut target: Option<lifecycle::TargetKey> = None;
    if let Some(entry) = &initial {
        println!(
            "source: \"{}\" ({}x{}, class {})",
            entry.title, entry.width, entry.height, entry.class
        );
        let t = lifecycle::TargetKey::of(entry);
        if let Some(p) = profile_for(&stored, &t) {
            println!("source profile: {}", t.describe());
            profile = p;
            for w in profile.apply_to(&mut params) {
                eprintln!("warning: {w}");
            }
        }
        target = Some(t);
    }
    args.apply_source(&mut params)?;
    // The command-line crop and divisor win over the profile's, until the Source section changes them (a
    // detected crop, a reset, a multiplier: then the profile holds the choice).
    let mut cli_crop = args.crop;
    let mut cli_divisor = args.divisor;
    let resolve_map = |params: &crate::params::Params,
                       profile: &settings::SourceProfile,
                       crop: Option<scaling::Rect>,
                       divisor: Option<(u32, u32)>| {
        scaling::SourceMap::resolve(params, crop.or(profile.crop_rect()), divisor.or(profile.divisor_pair()))
    };
    let (mut map, warnings) = resolve_map(&params, &profile, cli_crop, cli_divisor);
    for w in &warnings {
        eprintln!("warning: {w}");
    }
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
            initial.as_ref().map(|e| hwnd_of(e.hwnd)),
            stored.global.output.windowed,
        ),
    );
    let mut capture = match &initial {
        None => None,
        Some(entry) => match capture::Capture::start(hwnd_of(entry.hwnd), map.client_area, guess.capture_luid()) {
            Ok(c) => {
                println!("capture: Windows.Graphics.Capture on {} (CPU copy)", c.adapter);
                Some(c)
            }
            // The developer path stops on the error, as before; the last choice falls back to the picker.
            Err(e) if args.window.is_some() => return Err(e.into()),
            Err(e) => {
                start_message = Some((false, format!("The last window could not be captured: {e}")));
                None
            }
        },
    };
    let mut frame = capture::Frame::default();
    let synthetic_frame = |frame: &mut capture::Frame, n: &mut u64| -> Res<bool> {
        if *n == 0 || kind.is_animated() {
            if let Some(f) = kind.frame(*n)? {
                *frame = f;
            }
            *n += 1;
            return Ok(true);
        }
        Ok(false)
    };

    if let Some(path) = &args.save_capture {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let got = match &capture {
                Some(c) => c.take_frame(&mut frame),
                None => synthetic_frame(&mut frame, &mut synthetic_n)?,
            };
            if got {
                break;
            }
            if Instant::now() > deadline {
                return Err(NO_FRAME.into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        save_png(path, &frame)?;
        println!("saved {}x{} capture to {path}", frame.width, frame.height);
        return Ok(());
    }

    // Wait for the first frame to size the window. Without a source: the idle frame and the picker.
    if capture.is_some() || synthetic {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let got = match &capture {
                Some(c) => c.take_frame(&mut frame),
                None => synthetic_frame(&mut frame, &mut synthetic_n)?,
            };
            if got {
                break;
            }
            if Instant::now() > deadline {
                if args.window.is_some() || synthetic {
                    return Err(NO_FRAME.into());
                }
                // The last choice does not draw: the picker instead.
                start_message = Some((false, format!("The last window did not draw: {NO_FRAME}.")));
                capture = None;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    if capture.is_none() && !synthetic {
        target = None;
        frame = idle_frame();
        // No profile without a source: the parameters of the preset in force.
        profile = settings::SourceProfile::default();
        profile.switch_to(&mut params, &start_base);
        args.apply_source(&mut params)?;
        map = resolve_map(&params, &profile, cli_crop, cli_divisor).0;
    }
    // Initial windowed size (D-18: no assumed host resolution): the largest whole multiple of the frame
    // within 5/6 of the height and the width of the chosen (else the first) monitor. Without a source, two
    // thirds of the monitor.
    let monitors = output::monitor_rects();
    let screen = monitors
        .get(args.monitor.unwrap_or(0))
        .or(monitors.first())
        .map_or((1280, 720), |r| (r.right - r.left, r.bottom - r.top));
    let fit = |avail: i32, size: u32| (avail.max(1) as u32 * 5 / 6) / size.max(1);
    let windowed_size = if capture.is_some() || synthetic {
        let scale = fit(screen.1, frame.height).min(fit(screen.0, frame.width)).max(1) as i32;
        (frame.width as i32 * scale, frame.height as i32 * scale)
    } else {
        ((screen.0 * 2 / 3).max(960), (screen.1 * 2 / 3).max(640))
    };
    let (bindings, hotkey_warnings) = hotkeys::Bindings::from_settings(&stored.global.hotkeys);
    for w in &hotkey_warnings {
        eprintln!("warning: {w}");
    }
    let follow = initial
        .as_ref()
        .filter(|_| capture.is_some())
        .map_or(HWND::default(), |e| hwnd_of(e.hwnd));
    let win = output::create(&output::Options {
        fullscreen: args.fullscreen,
        monitor: args.monitor,
        follow,
        client_size: windowed_size,
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
    // The output never takes the keyboard (OUT-2), except while the menu or the picker is shown.
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
    // Lost source (T-56, A23): reminders, reattach during the wait, then the picker (or an exit).
    let policy = lifecycle::LostPolicy {
        wait_s: if args.wait_source_set {
            args.lost.wait_s
        } else {
            stored.global.source.wait_s
        },
        exit_on_lost: args.lost.exit_on_lost,
    };
    let mut lost_watch = lifecycle::LostWatch::new(policy, args.fullscreen);
    let mut lost_at: Option<f64> = None;
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
    let baseline_of = |library: &presets::Library,
                       preset: Option<&str>,
                       profile: &settings::SourceProfile|
     -> crate::params::Params {
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
    let mut baseline = baseline_of(&library, current_preset.as_deref(), &profile);
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
    // A23: the parameters a new source starts from: the preset in force with the global overrides, no profile.
    let base_params = |live: &live::Live| -> crate::params::Params {
        match live.current_preset().and_then(|id| live.library().find(id)) {
            Some(p) => {
                let mut b = p.params();
                let _ = settings::apply_overrides(&mut b, &live.settings().global.overrides);
                b
            }
            None => start_base,
        }
    };
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

    // A23: the source state machine and the picker's upkeep.
    let mut phase = if capture.is_some() || synthetic {
        Phase::Attached
    } else {
        Phase::NoSource
    };
    let mut reattach = match (&capture, &target) {
        (Some(_), Some(t)) => Some(lifecycle::Reattacher::new(t.clone(), follow.0 as isize)),
        _ => None,
    };
    if let (Some(_), Some(t)) = (&capture, &target) {
        live.remember_source(settings::LastSource {
            exe: t.exe.clone().unwrap_or_default(),
            class: t.class.clone(),
            title: t.title.clone(),
        });
    }
    // The name in the title bar: the program attached (a pattern or an image: its description).
    let mut source_name: Option<String> = match (&target, synthetic) {
        (Some(t), _) if capture.is_some() => Some(target_name(t)),
        (_, true) => Some(
            args.pattern
                .as_deref()
                .map(|p| format!("pattern {p}"))
                .or(args.image.as_deref().map(|i| format!("image {i}")))
                .unwrap_or_default(),
        ),
        _ => None,
    };
    output::set_title(win.hwnd, &window_title(source_name.as_deref(), None));
    ui.set_picker_message(start_message.take());
    let mut source_request: Option<SourceAction> = None;
    // The Source section's black-bar detection, crop and multiplier edits; `remap` recomputes the view (native
    // size, placement, readouts) at the next frame even when the source sends none.
    let mut bars_state = BarsState::default();
    let mut remap = false;
    if capture.is_some() && live.settings().global.source.detect_bars_at_attach {
        bars_state.session = Some(bars::Session::new(
            start.elapsed().as_secs_f64(),
            live.settings().global.source.bars_config(),
        ));
        println!("black bars: detecting (detect at attach)");
    }
    let mut list_due = 0.0f64; // next enumeration of the picker's list
    let mut thumbs = picker::ThumbPlan::default();
    let mut thumb_capture: Option<(isize, capture::Capture)> = None;
    let own = win.hwnd.0 as isize;

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
        // SAFETY: plain query.
        let fg = unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() }.0 as isize;
        if fg != own {
            recent.note(fg);
        }

        // ---- A23: source requests of the picker and the menu ----
        let mut attach_request = None;
        let mut start_bars = false;
        let mut crop_request: Option<SourceAction> = None;
        let mut multiplier_request: Option<scaling::Multiplier> = None;
        match source_request.take() {
            None => {}
            Some(SourceAction::DetectBars) => start_bars = true,
            Some(a @ (SourceAction::ResetCrop | SourceAction::UndoCrop)) => crop_request = Some(a),
            Some(SourceAction::SetMultiplier(m)) => multiplier_request = Some(m),
            Some(SourceAction::Attach(h)) => attach_request = Some(h),
            Some(SourceAction::Cancel) => phase = picker::next(phase, Event::Cancel),
            Some(SourceAction::ChangeSource) => {
                phase = picker::next(phase, Event::ShowPicker);
                if !picker::follows_target(phase) {
                    // Change source while a lost source was waited for: the wait ends, nothing to go back to.
                    reattach = None;
                    lost_at = None;
                    lost_watch.update(now, true);
                    frame = idle_frame();
                    fresh = true;
                    source_name = None;
                    ui.source_gone();
                    ui.show_picker(false);
                }
                list_due = 0.0;
            }
            Some(SourceAction::Detach) => {
                if let Some(c) = capture.take() {
                    captured_base += c.frames_captured();
                }
                reattach = None;
                lost_at = None;
                lost_watch.update(now, true);
                phase = picker::next(phase, Event::Detach);
                synthetic = false;
                frame = idle_frame();
                fresh = true;
                size_watch.request_history_clear();
                ui.source_gone();
                output::set_source(HWND::default());
                source_name = None;
                bars_state.forget();
                output::set_title(win.hwnd, &window_title(None, None));
                println!("source: detached");
            }
            Some(SourceAction::Refresh) => list_due = 0.0,
            Some(SourceAction::RefreshPreview) => thumbs.refresh(),
        }
        if let Some(h) = attach_request {
            let entry = entry_of(h, &ui.picker().rows);
            let t = lifecycle::TargetKey::of(&entry);
            let new_profile = profile_for(live.settings(), &t).unwrap_or_default();
            let mut p = params;
            for w in new_profile.switch_to(&mut p, &base_params(&live)) {
                eprintln!("warning: {w}");
            }
            if let Err(e) = args.apply_source(&mut p) {
                eprintln!("warning: {e}");
            }
            let (new_map, warnings) = resolve_map(&p, &new_profile, cli_crop, cli_divisor);
            for w in &warnings {
                eprintln!("warning: {w}");
            }
            match capture::Capture::start(hwnd_of(h), new_map.client_area, capture_luid) {
                Ok(c) => {
                    if let Some(old) = capture.replace(c) {
                        captured_base += old.frames_captured();
                    }
                    frame.seq = 0; // the new session counts from 1
                    synthetic = false;
                    println!("source: attached \"{}\" ({})", entry.title, t.describe());
                    if new_profile != settings::SourceProfile::default() {
                        println!("source profile: {}", t.describe());
                    }
                    live.remember_source(settings::LastSource {
                        exe: t.exe.clone().unwrap_or_default(),
                        class: t.class.clone(),
                        title: t.title.clone(),
                    });
                    source_name = Some(target_name(&t));
                    output::set_title(win.hwnd, &window_title(source_name.as_deref(), None));
                    reattach = Some(lifecycle::Reattacher::new(t, h));
                    lost_at = None;
                    lost_watch.update(now, true);
                    profile = new_profile;
                    map = new_map;
                    params = p;
                    renderer.set_params(params);
                    baseline = baseline_of(live.library(), live.current_preset(), &profile);
                    health.reset(now);
                    smoothing.reset();
                    detector.reset();
                    detect_key = (0, 0, None);
                    remap = true;
                    // A new window: the command-line crop and divisor stay, the last detection is forgotten.
                    bars_state.forget();
                    if live.settings().global.source.detect_bars_at_attach {
                        bars_state.session = Some(bars::Session::new(now, live.settings().global.source.bars_config()));
                        println!("black bars: detecting (detect at attach)");
                    }
                    size_watch.request_history_clear();
                    output::set_source(hwnd_of(h));
                    phase = picker::next(phase, Event::Attached);
                    ui.attached(h);
                }
                Err(e) => {
                    phase = picker::next(phase, Event::AttachFailed);
                    let name = if entry.title.is_empty() {
                        "this window".to_string()
                    } else {
                        format!("\u{201C}{}\u{201D}", entry.title)
                    };
                    ui.set_picker_message(Some((false, format!("Cannot capture {name}: {e}"))));
                    eprintln!("warning: cannot capture \"{}\": {e}", entry.title);
                }
            }
        }
        if phase == Phase::NoSource {
            // Nothing captured: the picker (it takes the foreground, as the menu does).
            ui.source_gone();
            ui.show_picker(false);
            phase = picker::next(phase, Event::ShowPicker);
            list_due = 0.0;
        }

        // ---- Lifecycle of the attached window: reattach during the wait, then the picker ----
        if picker::follows_target(phase) {
            if let Some(r) = reattach.as_mut() {
                let closed = capture.as_ref().is_some_and(|c| c.is_closed());
                match r.update(now, closed, sources::window_exists, sources::list_entries) {
                    lifecycle::Step::Idle => {}
                    lifecycle::Step::Lost => {
                        captured_base += capture.as_ref().map_or(0, |c| c.frames_captured());
                        capture = None;
                        lost_at = Some(now);
                        phase = picker::next(phase, Event::SourceLost);
                        println!("source lost: waiting for {}", r.target().describe());
                    }
                    lifecycle::Step::TryAttach(hwnd) => {
                        match capture::Capture::start(hwnd_of(hwnd), map.client_area, capture_luid) {
                            Ok(c) => {
                                capture = Some(c);
                                r.attach_result(now, hwnd, true);
                                ui.set_source(Some(hwnd));
                                output::set_source(hwnd_of(hwnd));
                                frame.seq = 0; // the new session counts from 1
                                health.reset(now);
                                size_watch.request_history_clear();
                                lost_at = None;
                                phase = picker::next(phase, Event::Reattached);
                                source_name = Some(target_name(r.target()));
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
            if !picker::follows_target(phase) {
                // Lost while the picker was open (Change source): nothing to go back to.
                reattach = None;
                lost_at = None;
                lost_watch.update(now, true);
                output::set_source(HWND::default());
                ui.source_gone();
                ui.show_picker(false);
                frame = idle_frame();
                fresh = true;
                source_name = None;
            }
        }
        if let Some(r) = reattach.as_ref() {
            match lost_watch.update(now, r.is_attached()) {
                lifecycle::LostAction::None => {}
                lifecycle::LostAction::Remind => println!("{}", lifecycle::LOST_REMINDER),
                a @ (lifecycle::LostAction::GoWindowed | lifecycle::LostAction::GiveUp) => {
                    if a == lifecycle::LostAction::GoWindowed && live.go_windowed(win.hwnd) {
                        lost_watch.set_fullscreen(false);
                    }
                    let wait = lost_watch.policy().wait_s;
                    let what = target_name(r.target());
                    println!("source lost for {wait:.0} s: choose a window to capture");
                    ui.set_picker_message(Some((
                        false,
                        format!("{what} closed and did not come back within {wait:.0} s."),
                    )));
                    reattach = None;
                    lost_at = None;
                    lost_watch.update(now, true);
                    phase = picker::next(phase, Event::GiveUp);
                    frame = idle_frame();
                    fresh = true;
                    size_watch.request_history_clear();
                    output::set_source(HWND::default());
                    source_name = None;
                    output::set_title(win.hwnd, &window_title(None, None));
                }
                lifecycle::LostAction::Exit => {
                    println!("source lost: exiting (--exit-on-lost)");
                    break;
                }
            }
        }

        // ---- The picker's list, icons and preview (only while it is shown) ----
        if ui.picker_open() {
            if now >= list_due {
                list_due = now + 1.0;
                ui.set_picker_rows(picker_rows(&recent));
                for path in ui.missing_icons() {
                    let icon = sources::exe_icon(&path);
                    ui.set_icon(&path, icon);
                }
            }
            if let Some(h) = thumbs.update(now, ui.picker().wanted_preview(), |h| h == own) {
                match capture::Capture::start(hwnd_of(h), true, capture_luid) {
                    Ok(c) => thumb_capture = Some((h, c)),
                    Err(_) => {
                        thumbs.finished(h, now, false);
                        ui.set_thumbnail_failed(h);
                    }
                }
            }
            if let Some((h, c)) = &thumb_capture {
                let h = *h;
                let mut f = capture::Frame::default();
                if c.take_frame(&mut f) {
                    let (w, th, px) = picker::thumbnail(&f.data, f.width, f.height, 480, 270);
                    ui.set_thumbnail(h, w, th, &px);
                    thumbs.finished(h, now, true);
                    thumb_capture = None;
                } else if thumbs.timed_out(now) {
                    thumbs.finished(h, now, false);
                    ui.set_thumbnail_failed(h);
                    thumb_capture = None;
                }
            }
        } else if thumb_capture.is_some() || thumbs != picker::ThumbPlan::default() {
            thumb_capture = None;
            thumbs.reset();
        }

        // ---- Frames ----
        let got = match &capture {
            Some(c) => c.take_frame(&mut frame),
            None if synthetic => synthetic_frame(&mut frame, &mut synthetic_n)?,
            None => false,
        };
        if got {
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
                // The black-bar detection accumulates only while it runs (about one second).
                if let Some(s) = bars_state.session.as_mut() {
                    s.feed(&frame.data, frame.width, frame.height);
                }
            }
            fresh = true;
        }

        // ---- The Source section: black-bar detection, crop reset and undo, scaling multiplier ----
        if capture.is_none() {
            bars_state.session = None;
        }
        let mut edited = false;
        if start_bars && capture.is_some() && bars_state.session.is_none() {
            let mut session = bars::Session::new(now, live.settings().global.source.bars_config());
            if !map.client_area {
                // The detection works on the client area (the manual crop lies in it): the capture follows first.
                params.source_crop_mode = crate::params::SOURCE_CROP_MODE_AUTO;
                renderer.set_params(params);
                let (new_map, _) = resolve_map(&params, &profile, cli_crop, cli_divisor);
                if switch_map(
                    new_map,
                    &mut map,
                    &mut capture,
                    reattach.as_ref().map(|r| r.hwnd()),
                    &mut captured_base,
                    &mut frame.seq,
                    capture_luid,
                ) {
                    detect_key = (0, 0, None);
                    remap = true;
                }
            } else if frame.seq > 0 {
                // The frame on screen counts: a game that redraws rarely still gives one.
                session.feed(&frame.data, frame.width, frame.height);
            }
            bars_state.session = Some(session);
            bars_state.message = None;
            println!("black bars: detecting");
        }
        if let Some(outcome) = bars_state
            .session
            .as_ref()
            .filter(|s| s.done(now))
            .map(|s| s.outcome(view.div))
        {
            bars_state.session = None;
            let (_, text) = outcome.message();
            println!("black bars: {text}");
            if let bars::Outcome::Found(found) = outcome {
                let (b, p) = (found.bars, found.picture);
                println!(
                    "black bars: left {}, top {}, right {}, bottom {} px; crop {}, {}, {}x{}",
                    b.left, b.top, b.right, b.bottom, p.x, p.y, p.w, p.h
                );
                bars_state.undo = Some(bars::CropState::capture(&params, &profile, cli_crop));
                bars::set_manual_crop(&mut params, &mut profile, &mut cli_crop, found.picture);
                bars_state.found = Some(found);
                edited = true;
            }
            bars_state.message = Some(outcome.message());
        }
        match crop_request {
            Some(SourceAction::ResetCrop) => {
                bars_state.undo = Some(bars::CropState::capture(&params, &profile, cli_crop));
                bars::reset_crop(&mut params, &mut profile, &mut cli_crop);
                bars_state.found = None;
                bars_state.message = Some((true, "Crop reset: the whole client area is captured.".to_string()));
                println!("source: crop reset to the client area");
                edited = true;
            }
            Some(SourceAction::UndoCrop) => {
                if let Some(u) = bars_state.undo.take() {
                    u.restore(&mut params, &mut profile, &mut cli_crop);
                    bars_state.found = None;
                    bars_state.message = Some((true, "The last crop change was undone.".to_string()));
                    println!("source: crop change undone");
                    edited = true;
                }
            }
            _ => {}
        }
        if let Some(m) = multiplier_request {
            m.apply(&mut params, &mut profile);
            cli_divisor = None;
            println!(
                "source scaling multiplier: {}",
                match m {
                    scaling::Multiplier::Auto => "auto".to_string(),
                    scaling::Multiplier::None => "none (native)".to_string(),
                    scaling::Multiplier::Manual(x, y) => scaling::Multiplier::pair_text((x, y)),
                }
            );
            edited = true;
        }
        if edited {
            // Live, and stored in the profile of this program and class.
            renderer.set_params(params);
            profile = keyed_profile(&profile, reattach.as_ref().map(|r| r.target()));
            live.save_source_profile(&profile);
            baseline = baseline_of(live.library(), live.current_preset(), &profile);
            let (new_map, warnings) = resolve_map(&params, &profile, cli_crop, cli_divisor);
            for w in &warnings {
                eprintln!("warning: {w}");
            }
            if switch_map(
                new_map,
                &mut map,
                &mut capture,
                reattach.as_ref().map(|r| r.hwnd()),
                &mut captured_base,
                &mut frame.seq,
                capture_luid,
            ) {
                // The integer-scale detection runs again on the new area.
                detect_key = (0, 0, None);
            }
            remap = true;
        }
        if std::mem::take(&mut remap) {
            fresh = true;
        }
        if fresh {
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
            // Patterns, images and the idle frame are native.
            let fixed = map.divisor.or((synthetic || capture.is_none()).then_some((1, 1)));
            // A capture is always analysed (until the detection locks), also under a forced multiplier, so the
            // Source section shows what was detected next to the forced value.
            if capture.is_some() {
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
                    if let Some(f) = map.divisor.filter(|&f| f != d) {
                        println!(
                            "source scaling: forced {} ({} detected)",
                            scaling::Multiplier::pair_text(f),
                            scaling::Multiplier::pair_text(d)
                        );
                    }
                }
            }
            let div = fixed.unwrap_or_else(|| detector.divisor());
            view = scaling::View::new(region, div);
            // SRC-2: a detected (not given) divisor of 1 on a locked detection means no integer scale.
            // That alone also holds for text at 1:1, so the watch's verdict needs filtered edges on a
            // regular grid (an upscale of a low-resolution source), see `lifecycle::EdgeStats`.
            let eligible = capture.is_some() && map.divisor.is_none() && detector.is_locked() && div == (1, 1);
            if got && smoothing.feed(&frame.data, frame.width, region, eligible) {
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
            baseline = baseline_of(live.library(), stepped_preset.as_deref(), &profile);
        }
        if ui.is_open() || ui.overlay_on() {
            ui_state.params = params;
            // The preset in force, also when a preset hotkey stepped to it.
            ui_state.preset = live.current_preset().map(str::to_string);
            ui_state.baseline = Some(baseline);
            ui_state.headroom = renderer.headroom();
            ui_state.placement = Some(placement::readout(&params, view.native, out_size, args.integer));
            ui_state.source = source_view(
                capture.is_some(),
                reattach.as_ref().map(|r| r.target()),
                &profile,
                &map,
                &detector,
                (frame.width, frame.height),
                view,
                lost_at.map(|t| (now - t, lost_watch.policy().wait_s)),
                scaling::Multiplier::of(&params, cli_divisor.or(profile.divisor_pair())),
                bars_state.view(),
            );
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
                baseline = baseline_of(live.library(), changes.preset.as_deref(), &profile);
                // A saved user preset keeps the edits made before the save: recorded against the new preset.
                live.note_params_edit(&baseline, &p, &baseline);
                ui_state.preset = changes.preset;
            } else {
                live.note_params_edit(&params, &p, &baseline);
            }
            params = p;
            renderer.set_params(params);
            // T-96, A23: the crop and divisor modes are live: the source map follows them (a change of the
            // capture area restarts the session on the same window).
            let (new_map, _) = resolve_map(&params, &profile, cli_crop, cli_divisor);
            if switch_map(
                new_map,
                &mut map,
                &mut capture,
                reattach.as_ref().map(|r| r.hwnd()),
                &mut captured_base,
                &mut frame.seq,
                capture_luid,
            ) {
                detect_key = (0, 0, None);
                remap = true;
            }
        }
        if let Some(adapter) = changes.gpu_adapter {
            println!("gpu: adapter for the next start set to \"{adapter}\" (restart to apply)");
            live.set_gpu_adapter(adapter);
        }
        if let Some(SourcePrefs {
            ask_at_start,
            wait_s,
            detect_bars_at_attach,
            bars: bars_config,
        }) = changes.source_prefs
        {
            live.set_source_prefs(ask_at_start, wait_s);
            live.set_bars_prefs(detect_bars_at_attach, bars_config);
            if !args.wait_source_set {
                lost_watch.set_wait(wait_s);
            }
        }
        if changes.source.is_some() {
            source_request = changes.source;
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
            } else if capture.is_none() {
                None
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
            let rates = (source_name.is_some()).then(|| {
                format!(
                    "capture {:.0} fps | render {:.0} fps | source {}x{} (div {}x{}) | {}{}",
                    (captured - last_captured) as f64 / secs,
                    renders as f64 / secs,
                    view.native.0,
                    view.native.1,
                    view.div.0,
                    view.div.1,
                    renderer.gpu_name,
                    notice.as_ref().map(|m| format!(" | {m}")).unwrap_or_default()
                )
            });
            output::set_title(win.hwnd, &window_title(source_name.as_deref(), rates));
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
    drop(thumb_capture);
    drop(ui);
    drop(renderer);
    drop(capture);
    Ok(())
}

/// Switches the source map to `new_map`. A change of the capture area (client area or whole window) restarts the
/// capture session on the attached window `hwnd`. Returns whether the map changed (the caller restarts the
/// integer-scale detection and recomputes the view).
fn switch_map(
    new_map: scaling::SourceMap,
    map: &mut scaling::SourceMap,
    capture: &mut Option<capture::Capture>,
    hwnd: Option<isize>,
    captured_base: &mut u64,
    seq: &mut u64,
    luid: Option<adapters::Luid>,
) -> bool {
    if new_map == *map {
        return false;
    }
    if new_map.client_area != map.client_area {
        if let (Some(h), true) = (hwnd, capture.is_some()) {
            match capture::Capture::start(hwnd_of(h), new_map.client_area, luid) {
                Ok(c) => {
                    if let Some(old) = capture.replace(c) {
                        *captured_base += old.frames_captured();
                    }
                    *seq = 0; // the new session counts from 1
                }
                Err(e) => eprintln!("warning: capture area change: {e}"),
            }
        }
    }
    *map = new_map;
    true
}

/// The black-bar detection state the Source section shows and the Undo it offers.
#[derive(Default)]
struct BarsState {
    /// The running detection, if any.
    session: Option<bars::Session>,
    /// The crop before the last detection or reset (Undo).
    undo: Option<bars::CropState>,
    /// The last detection that found bars (cleared by a reset or an undo).
    found: Option<bars::Found>,
    /// The last outcome, reset or undo, in words.
    message: Option<(bool, String)>,
}

impl BarsState {
    fn view(&self) -> crate::ui::BarsView {
        crate::ui::BarsView {
            running: self.session.is_some(),
            found: self.found,
            message: self.message.clone(),
            can_undo: self.undo.is_some(),
        }
    }

    /// A new window: nothing to undo, no outcome to show.
    fn forget(&mut self) {
        *self = BarsState::default();
    }
}

/// The source profile of the attached program and class with the key filled in (a window without a profile yet
/// starts from an empty one), for [`live::Live::save_source_profile`].
fn keyed_profile(profile: &settings::SourceProfile, target: Option<&lifecycle::TargetKey>) -> settings::SourceProfile {
    let mut p = profile.clone();
    if let Some((exe, class)) = target.and_then(|t| t.profile_key()) {
        p.exe = exe.to_string();
        p.class = class.to_string();
    }
    p
}

/// What the menu's Source section shows (A23).
#[allow(clippy::too_many_arguments)]
fn source_view(
    capturing: bool,
    target: Option<&lifecycle::TargetKey>,
    profile: &settings::SourceProfile,
    map: &scaling::SourceMap,
    detector: &scaling::Detector,
    frame: (u32, u32),
    view: scaling::View,
    lost: Option<(f64, f64)>,
    multiplier: scaling::Multiplier,
    bars: crate::ui::BarsView,
) -> SourceView {
    let waiting = match (target, lost) {
        (Some(t), Some((since, wait))) => Some(if wait > 0.0 {
            format!(
                "{} closed: waiting for it to come back ({:.0} s left), then the window list opens.",
                t.describe(),
                (wait - since).max(0.0)
            )
        } else {
            format!("{} closed: waiting for it to come back.", t.describe())
        }),
        _ => None,
    };
    let attached = target.filter(|_| capturing).map(|t| AttachedView {
        exe: t.exe.clone(),
        class: t.class.clone(),
        title: t.title.clone(),
        frame,
        native: view.native,
        scale: scale_text(map, detector, view.div),
        crop: crop_text(map),
        profile: *profile != settings::SourceProfile::default(),
        multiplier,
        detected: detector.is_locked().then(|| detector.divisor()),
        crop_manual: map.crop.is_some() || !map.client_area,
        bars,
    });
    SourceView { attached, waiting }
}

/// The integer scale in words for the Source section.
pub fn scale_text(map: &scaling::SourceMap, detector: &scaling::Detector, div: (u32, u32)) -> String {
    let pair = |d: (u32, u32)| {
        if d.0 == d.1 {
            format!("x{}", d.0)
        } else {
            format!("x{} \u{00D7} {}", d.0, d.1)
        }
    };
    match map.divisor {
        Some((1, 1)) => "none (x1)".to_string(),
        Some(d) if detector.is_locked() && detector.divisor() != d => {
            format!("{} (manual, detected {})", pair(d), pair(detector.divisor()))
        }
        Some(d) => format!("{} (manual)", pair(d)),
        None if !detector.is_locked() => "detecting...".to_string(),
        None if div == (1, 1) => "x1 (native)".to_string(),
        None => format!("{} detected", pair(div)),
    }
}

/// The capture area in words for the Source section.
pub fn crop_text(map: &scaling::SourceMap) -> String {
    match (map.client_area, map.crop) {
        (false, _) => "whole window".to_string(),
        (true, Some(c)) => format!("crop {}, {}, {} \u{00D7} {}", c.x, c.y, c.w, c.h),
        (true, None) => "client area".to_string(),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_name_the_program_or_invite_to_choose() {
        assert_eq!(window_title(None, None), "ultimateCRT | choose a window to capture");
        assert_eq!(
            window_title(Some("pixel-demo.exe - pixel-demo"), Some("capture 60 fps".into())),
            "ultimateCRT | pixel-demo.exe - pixel-demo | capture 60 fps"
        );
        let t = lifecycle::TargetKey {
            exe: Some("Mina.exe".into()),
            class: "C".into(),
            title: "Mina".into(),
        };
        assert_eq!(target_name(&t), "Mina.exe - Mina");
        let u = lifecycle::TargetKey {
            exe: None,
            class: "C".into(),
            title: String::new(),
        };
        assert_eq!(target_name(&u), "unknown program");
    }

    #[test]
    fn the_source_section_words() {
        let auto = scaling::SourceMap {
            client_area: true,
            crop: None,
            divisor: None,
        };
        let d = scaling::Detector::default();
        assert_eq!(scale_text(&auto, &d, (1, 1)), "detecting...");
        let manual = scaling::SourceMap {
            divisor: Some((2, 3)),
            ..auto
        };
        assert_eq!(scale_text(&manual, &d, (2, 3)), "x2 \u{00D7} 3 (manual)");
        let none = scaling::SourceMap {
            divisor: Some((1, 1)),
            ..auto
        };
        assert_eq!(scale_text(&none, &d, (1, 1)), "none (x1)");
        assert_eq!(crop_text(&auto), "client area");
        let whole = scaling::SourceMap {
            client_area: false,
            ..auto
        };
        assert_eq!(crop_text(&whole), "whole window");
        let crop = scaling::SourceMap {
            crop: Some(scaling::Rect {
                x: 8,
                y: 4,
                w: 320,
                h: 240,
            }),
            ..auto
        };
        assert_eq!(crop_text(&crop), "crop 8, 4, 320 \u{00D7} 240");
        assert_eq!(idle_frame().data.len(), (IDLE_SIZE.0 * IDLE_SIZE.1 * 4) as usize);
    }
}
