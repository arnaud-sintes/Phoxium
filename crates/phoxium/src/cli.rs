//! Command line: the usage text, the parsed options and the parser.

use std::env;

use crate::params::{Applied, ParamValue, Params};
use crate::{lifecycle, patterns, scaling, Res};

pub const USAGE: &str = "phoxium: captures a window and shows it in a second window through a GPU pipeline.

Without options the output window opens and lists the windows to capture: choose one there (Enter or
double-click). Change source and Detach are in the settings menu (F10). The options below are developer and
diagnostic conveniences.

Usage: phoxium [options]
  --window <text>      capture the window whose title starts with <text>, else contains it, at once
                       (no picker; an error when there is none)
  --process <exe>      capture a window of this program (its executable name, with or without .exe, any case): the
                       largest one; with --window also with that text in its title. The saved source profile of
                       the program applies by itself. For launchers that know the program, not the title
  --wait-window <secs> with --window or --process: wait this long for the window to appear (0 = no limit) instead
                       of failing at once, so a launcher can start Phoxium before the game window exists
  --list-windows       list capturable windows and monitors, then exit
  --video <text>       use the video capture device (capture card, camera) whose name starts with <text>, else
                       contains it, at once: no picker (an error listing the devices when there is none). Not with
                       --window, --process, --pattern or --image
  --list-video         list the video capture devices, then exit
  --list-gpus          list Vulkan devices, then exit
  --list-adapters      adapter topology (DXGI adapters and monitors, Vulkan devices, LUIDs, the chosen adapter,
                       capture adapter, verdict, warnings), then exit; opens no window
  --gpu <index>        Vulkan device (see --list-gpus) that runs the chain, capture included; wins over the
                       settings file's [global.gpu] adapter (default auto: the adapter driving the monitor the
                       output covers)
  --capture-path <auto|zero-copy|cpu>  how frames reach the renderer: zero-copy GPU sharing (auto, the default:
                       used when capture and rendering share one adapter that supports it, else the CPU copy, with
                       an automatic fallback on any failure) or the CPU copy (most compatible); wins over
                       [global.capture] path
  --fullscreen         borderless, topmost, non-activating output covering the source's monitor
  --monitor <index>    monitor to cover with --fullscreen (see --list-windows; default: the source's)
  --no-activate        accepted for older scripts, no effect: every output is non-activating
  --no-crop            capture the whole window instead of its client area
  --crop <x,y,w,h>     part of the captured client area that is the source (default: the picture area found
                       inside black bars, see the Source section)
  --divisor <N|NxM>    the source is an integer upscale by N (or N horizontally, M vertically);
                       1 means native. Default: detected from the first informative frames
  --integer            tube scale mode: whole host pixels per source row only (letterboxed)
  --crop-mode <auto|manual|none>  source area: the client area with its picture area (auto: found inside
                       black bars, or all of it when the profile says so), the --crop rectangle or the source
                       profile's (manual), or the whole window (none). --crop implies manual, --no-crop none
  --divisor-mode <auto|manual|none>  native raster: detected (auto), --divisor or the profile's (manual),
                       or the captured pixels (none, divisor 1). --divisor implies manual
  --scale-mode <tube|native|integer|fit|fit-free|manual>  picture size: by the tube (default, physical), 1:1
                       host pixels per source pixel, xN (--scale), the largest whole factor that fits, the
                       largest non-integer fit (breaks the scanline and mask alignment), or a fractional
                       size set in the menu (manual, `source_pixel_size`)
  --scale <N>          host pixels per source row in the integer mode: 0 = largest that fits (default),
                       1 to 10 manual
  --align <center|top-left>  picture position in the output (default center)
  --surround <r,g,b>   colour outside the picture, output code values 0 to 1 (default 0,0,0: black)
  --par <W:H|number>   pixel aspect ratio of a source pixel (1:1 default, 8:7 NES/SNES, 32:35 Genesis H40)
  --aspect <par|fill|W:H|number>  picture shape: from the PAR (default), the whole tube (fill), or a forced
                       display aspect such as 4:3
  --scan <off|rolling|strobe>  motion clarity (`scan_mode`): spread each source frame over the screen refreshes, as a tube
                       draws it (rolling band of lines) or as black frame insertion (strobe); needs 120 Hz or more
  --source-colour <target[:variant]>  colour target: the colours of the source as the device it came from would
                       have shown them (`source_colour_target`), none or a target of the Source section (nintendo_game_boy_color,
                       nintendo_super_game_boy, sega_mega_drive...); after a colon its variant (`source_colour_variant`): a
                       number from 1, or the variant's id or name as the menu shows it (nintendo_super_game_boy:2-a,
                       sega_mega_drive:regen). Without a variant, the first. Wins over the settings and the profile
  --pattern <name[:args]>  generated test pattern instead of a capture (no window needed): flat[:level],
                       ramp, vline[:N], hline[:N], grating[:N], checker[:N], crosshatch[:N], point,
                       window[:APL%], bar[:width[:px-per-frame]], pluge, moire[:N]
  --pattern-size <WxH> size of the pattern (default 320x240)
  --image <file.png>   still PNG image instead of a capture (divisor 1)
  --preset <id|file.toml>  tube preset to start with: a shipped or user preset id, or a preset file (default:
                       the preset of the settings file, else the parameter table defaults); the parameter
                       overrides of the settings file apply on top
  --validation         enable the Vulkan validation layer (needs the Vulkan SDK)
  --stats             print the resolved placement (scale, picture, px per mm, px per triad, mask tier),
                       then capture and render rates every second
  --log <file.csv>     write a diagnostics CSV: one row per second (capture and render fps, min/max/p99 intervals,
                       gaps over 50 ms, foreground window changes) and a summary at exit
  --save-capture <png>save the first captured frame and exit
  --snapshot <png>     save a snapshot of the output (the picture the CRT drew) to this PNG after about a second, then exit
  --detect-bars <png>  offline: run the black-bar (picture area) detector on a saved capture (a --save-capture
                       PNG) with the settings' thresholds, print the picture found, the bars, the reason when
                       nothing is cropped and the per-edge statistics, then exit; opens no window
  --wait-source <secs> when the captured window closes: reattach to a new window of the same program and
                       class for this many seconds, then show the window list again (a fullscreen output
                       becomes a normal window). Default: [global.source] wait_s of the settings (30);
                       0 = wait forever
  --exit-on-lost       exit when the captured window closes instead of waiting for it
  --exit-after <secs>  quit after some seconds (for automated tests)
  --allow-multiple     do not refuse to start when another Phoxium runs (one at a time by default: two would
                       share the settings and the burn-in state); runs with --exit-after, --snapshot or
                       --save-capture are never refused
  --reset-burnin       start the burn-in wear map of the tube preset as a new tube (the saved wear is replaced
                       at the next save); the same as the menu button \"New tube\"
  render ...           (first argument) offline render of a pattern or image to PNG or PFM, no window; see `phoxium render --help`
  --version            print the version and exit
  --help               print this text

The output does not take the keyboard focus. Hotkeys are global and configurable in settings.toml
[global.hotkeys] or in the settings menu (Quit included); the defaults follow. No default uses Ctrl+Alt with a
letter: that also fires on AltGr. A chord another program owns falls back and is reported.";

/// The default hotkeys, one per line, from the table of the actions (`hotkeys::Action`): never a copy of the chords.
pub fn hotkeys_help() -> String {
    let mut s = String::from("Default hotkeys:");
    for a in crate::hotkeys::Action::ALL {
        s.push_str(&format!("\n  {:<14} {}", a.default_text(), a.describe()));
    }
    s
}

#[derive(Default)]
pub struct Args {
    pub window: Option<String>,
    /// `--process <exe>`: capture a window of this program (see [`crate::sources::find_process_in`]).
    pub process: Option<String>,
    /// `--wait-window <secs>`: how long to wait at start for the `--window`/`--process` window (0 = no limit).
    pub wait_window: Option<f64>,
    pub list_windows: bool,
    /// `--video <text>`: the video capture device to open at start, by the rule of `--window` ([`crate::video::find_device`]).
    pub video: Option<String>,
    pub list_video: bool,
    pub list_gpus: bool,
    pub list_adapters: bool,
    pub gpu: Option<usize>,
    /// `--capture-path`: wins over `[global.capture] path` (A29).
    pub capture_path: Option<crate::zerocopy::CapturePath>,
    pub fullscreen: bool,
    pub monitor: Option<usize>,
    /// `--no-activate`: kept for older scripts, no effect (every output is non-activating, OUT-2).
    pub no_activate: bool,
    pub no_crop: bool,
    pub crop: Option<scaling::Rect>,
    pub divisor: Option<(u32, u32)>,
    pub integer: bool,
    pub validation: bool,
    pub stats: bool,
    pub save_capture: Option<String>,
    /// `--snapshot <png>`: the output as a PNG after a while, then exit (a developer check of the snapshot path).
    pub snapshot: Option<String>,
    /// `--detect-bars <png>`: the offline picture-area diagnostic.
    pub detect_bars: Option<String>,
    pub exit_after: Option<f64>,
    /// `--allow-multiple`: skip the one-instance guard (see [`crate::instance`]).
    pub allow_multiple: bool,
    /// `--reset-burnin`: the burn-in wear map starts as a new tube (T-43).
    pub reset_burnin: bool,
    pub lost: lifecycle::LostPolicy,
    /// `--wait-source` was given: it wins over `[global.source] wait_s` (A23).
    pub wait_source_set: bool,
    pub log: Option<String>,
    pub pattern: Option<String>,
    pub pattern_size: Option<(u32, u32)>,
    pub image: Option<String>,
    pub preset: Option<String>,
    /// T-96 source adjustment: enum value names of `source_crop_mode`, `source_divisor_mode`,
    /// `source_scale_mode` (checked when applied), and the other source parameters.
    pub crop_mode: Option<String>,
    pub divisor_mode: Option<String>,
    pub scale_mode: Option<String>,
    pub scale: Option<i32>,
    pub center: Option<bool>,
    pub surround: Option<[f32; 3]>,
    pub par: Option<f32>,
    pub aspect: Option<Aspect>,
    /// `--source-colour <target[:variant]>`: the colour target and variant, checked against the data files at parse.
    pub colour: Option<ColourChoice>,
    /// `--scan <off|rolling|strobe>`: the value of `scan_mode`, checked when applied.
    pub scan: Option<String>,
}

/// The `--source-colour` choice: a value of `source_colour_target` and the number of `source_colour_variant`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColourChoice {
    /// The value of `source_colour_target` (0 = none, then the data files in the order of `params/source.toml`).
    pub target: u32,
    /// The variant number, from 1 (1 when the text names no variant).
    pub variant: i32,
}

impl ColourChoice {
    /// Sets `source_colour_target` and `source_colour_variant` through the checked setter, as every other option does;
    /// `flag` names the option in the messages.
    pub fn apply(self, params: &mut Params, flag: &str) -> Res<()> {
        set_flag(params, flag, "source_colour_target", ParamValue::Enum(self.target))?;
        set_flag(params, flag, "source_colour_variant", ParamValue::I32(self.variant))
    }
}

/// How a name is compared: lower case, with the spaces and dashes read as `_` (`Genesis Plus GX`, `2-A`).
fn name_key(text: &str) -> String {
    text.trim().to_lowercase().replace([' ', '-'], "_")
}

/// The 0-based index of the variant of `target` that `text` names: a position from 1 when the text is digits only and
/// within the variants, else an id or a name ([`name_key`] on both sides), which is how a variant whose id is a
/// number too large to be a position (`800` the Atari 800, `24` degrees) is reached. `None` when nothing matches.
pub fn variant_by_text(target: &crate::targets::Target, text: &str) -> Option<usize> {
    let text = text.trim();
    if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) {
        if let Some(n) = text
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=target.variants.len()).contains(n))
        {
            return Some(n - 1);
        }
    }
    let key = name_key(text);
    target
        .variants
        .iter()
        .position(|v| name_key(&v.id) == key || name_key(&v.name) == key)
}

/// Reads `<target>[:<variant>]`: the target is a value name of `source_colour_target` (case and `-` or `_` free),
/// the variant a number from 1 or an id or name of the target's data file ([`variant_by_text`]). No variant means
/// the first. A target with no variants takes none; an unknown target or variant is an error listing what is accepted.
pub fn parse_colour(flag: &str, text: &str) -> Res<ColourChoice> {
    let values = crate::params::meta("source_colour_target").map_or(&[][..], |m| m.values);
    let (name, variant) = match text.split_once(':') {
        Some((n, v)) => (n, Some(v)),
        None => (text, None),
    };
    let wanted = name_key(name);
    let Some(target) = values.iter().position(|v| *v == wanted) else {
        return Err(format!(
            "{flag} expects <target>[:<variant>] with a target of {}, got \"{text}\"",
            values.join(", ")
        )
        .into());
    };
    let choice = |variant| ColourChoice {
        target: target as u32,
        variant,
    };
    let Some(variant) = variant else {
        return Ok(choice(1));
    };
    let Some(t) = crate::targets::for_value(target as u32).filter(|t| !t.variants.is_empty()) else {
        return Err(format!("{flag}: \"{}\" has no variants, got \"{variant}\"", values[target]).into());
    };
    match variant_by_text(t, variant) {
        Some(i) => Ok(choice(i as i32 + 1)),
        None => {
            let ids: Vec<&str> = t.variants.iter().map(|v| v.id.as_str()).collect();
            Err(format!(
                "{flag}: \"{}\" has no variant \"{variant}\" (a number from 1 to {}, or {})",
                t.id,
                ids.len(),
                ids.join(", ")
            )
            .into())
        }
    }
}

/// The `--aspect` choice (T-96).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Aspect {
    /// From the PAR (`raster_aspect` par, no forced aspect).
    Par,
    /// The whole tube (`raster_aspect` fill_tube).
    Fill,
    /// A forced display aspect (`source_display_aspect`).
    Display(f32),
}

fn number<T: std::str::FromStr>(flag: &str, text: &str) -> Res<T> {
    text.parse()
        .map_err(|_| format!("{flag} expects a number, got \"{text}\"").into())
}

/// A ratio "W:H" or a positive number.
pub fn parse_ratio(flag: &str, text: &str) -> Res<f32> {
    let v = match text.split_once(':') {
        Some((a, b)) => number::<f32>(flag, a.trim())? / number::<f32>(flag, b.trim())?,
        None => number::<f32>(flag, text.trim())?,
    };
    if v.is_finite() && v > 0.0 {
        Ok(v)
    } else {
        Err(format!("{flag} expects a positive ratio such as 8:7, got \"{text}\"").into())
    }
}

impl Args {
    /// The command line names the window to capture (`--window` or `--process`): no picker, and a window that cannot
    /// be captured stops the program (the developer path) instead of falling back to the picker.
    pub fn targeted(&self) -> bool {
        self.window.is_some() || self.process.is_some()
    }

    /// The window the command line asks for, in words (for messages).
    pub fn target_text(&self) -> String {
        match (&self.process, &self.window) {
            (Some(p), Some(t)) => format!("a window of the program \"{p}\" with \"{t}\" in its title"),
            (Some(p), None) => format!("a window of the program \"{p}\""),
            (None, Some(t)) => format!("a visible window with \"{t}\" in its title"),
            (None, None) => "a window".to_string(),
        }
    }

    /// Applies the source options of the command line onto `params` (T-96): they override the preset, the
    /// global overrides and the source profile. `--crop` implies the manual crop mode, `--no-crop` none,
    /// `--divisor` the manual divisor mode, unless the mode is given.
    pub fn apply_source(&self, params: &mut Params) -> Res<()> {
        use crate::settings::set_enum_by_name;
        let crop_mode = self.crop_mode.clone().or_else(|| {
            if self.crop.is_some() {
                Some("manual".into())
            } else if self.no_crop {
                Some("none".into())
            } else {
                None
            }
        });
        let divisor_mode = self.divisor_mode.clone().or(self.divisor.map(|_| "manual".into()));
        for (flag, id, value) in [
            ("--crop-mode", "source_crop_mode", &crop_mode),
            ("--divisor-mode", "source_divisor_mode", &divisor_mode),
            ("--scale-mode", "source_scale_mode", &self.scale_mode),
            ("--scan", "scan_mode", &self.scan),
        ] {
            if let Some(v) = value {
                if !set_enum_by_name(params, id, v) {
                    return Err(format!("{flag}: unknown value \"{v}\"").into());
                }
            }
        }
        // Through the checked setter, like every other input path (review D-07): the hard ranges and the
        // cross-parameter rules apply; a clamp is a notice, a rejected value an error.
        if let Some(n) = self.scale {
            set_flag(params, "--scale", "source_scale", ParamValue::I32(n))?;
        }
        if let Some(c) = self.center {
            set_flag(params, "--align", "source_center", ParamValue::Bool(c))?;
        }
        if let Some(c) = self.surround {
            set_flag(params, "--surround", "source_surround_color", ParamValue::Vec3(c))?;
        }
        if let Some(p) = self.par {
            set_flag(params, "--par", "source_par", ParamValue::F32(p))?;
        }
        if let Some(c) = self.colour {
            c.apply(params, "--source-colour")?;
        }
        let par = ParamValue::Enum(crate::params::RASTER_ASPECT_PAR);
        match self.aspect {
            None => {}
            Some(Aspect::Par) => {
                set_flag(params, "--aspect", "raster_aspect", par)?;
                set_flag(params, "--aspect", "source_display_aspect", ParamValue::F32(0.0))?;
            }
            Some(Aspect::Fill) => set_flag(
                params,
                "--aspect",
                "raster_aspect",
                ParamValue::Enum(crate::params::RASTER_ASPECT_FILL_TUBE),
            )?,
            Some(Aspect::Display(a)) => {
                set_flag(params, "--aspect", "raster_aspect", par)?;
                set_flag(params, "--aspect", "source_display_aspect", ParamValue::F32(a))?;
            }
        }
        Ok(())
    }
}

/// Sets `id` to `value` for the command-line option `flag` through [`Params::set_checked`]: a clamp (hard
/// range or cross-parameter rule) is recorded as a notice naming the option, a rejected value is an error.
fn set_flag(params: &mut Params, flag: &str, id: &str, value: ParamValue) -> Res<()> {
    match params.set_checked(id, value) {
        Ok(Applied::Exact) => Ok(()),
        Ok(Applied::Clamped(stored)) => {
            crate::params::notify(format!("{flag}: {}", crate::params::clamp_message(id, value, stored)));
            Ok(())
        }
        Err(why) => Err(format!(
            "{flag}: value {} is rejected ({why})",
            crate::params::value_text(id, value)
        )
        .into()),
    }
}

pub fn parse_args() -> Res<Args> {
    parse_from(env::args().skip(1))
}

/// Parses the options (without the program name).
pub fn parse_from(args: impl IntoIterator<Item = String>) -> Res<Args> {
    let mut a = Args::default();
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        let mut value = |name: &str| it.next().ok_or_else(|| format!("{name} expects a value"));
        match arg.as_str() {
            "--window" => a.window = Some(value("--window")?),
            "--process" => a.process = Some(value("--process")?),
            "--wait-window" => {
                let secs = number::<f64>("--wait-window", &value("--wait-window")?)?;
                if !secs.is_finite() || secs < 0.0 {
                    return Err("--wait-window expects seconds, 0 or more".into());
                }
                a.wait_window = Some(secs);
            }
            "--list-windows" => a.list_windows = true,
            "--video" => a.video = Some(value("--video")?),
            "--list-video" => a.list_video = true,
            "--list-gpus" => a.list_gpus = true,
            "--list-adapters" | "--adapters" => a.list_adapters = true,
            "--gpu" => a.gpu = Some(number("--gpu", &value("--gpu")?)?),
            "--capture-path" => {
                let v = value("--capture-path")?;
                a.capture_path = Some(
                    crate::zerocopy::CapturePath::parse(&v)
                        .ok_or_else(|| format!("--capture-path expects auto, zero-copy or cpu, got \"{v}\""))?,
                );
            }
            "--fullscreen" => a.fullscreen = true,
            "--monitor" => a.monitor = Some(number("--monitor", &value("--monitor")?)?),
            "--no-activate" => a.no_activate = true,
            "--no-crop" => a.no_crop = true,
            "--crop" => a.crop = Some(value("--crop")?.parse()?),
            "--divisor" => a.divisor = Some(scaling::parse_divisor(&value("--divisor")?)?),
            "--integer" => a.integer = true,
            "--validation" => a.validation = true,
            "--stats" => a.stats = true,
            "--reset-burnin" => a.reset_burnin = true,
            "--save-capture" => a.save_capture = Some(value("--save-capture")?),
            "--snapshot" => a.snapshot = Some(value("--snapshot")?),
            "--detect-bars" => a.detect_bars = Some(value("--detect-bars")?),
            "--wait-source" => {
                let secs: f64 = number("--wait-source", &value("--wait-source")?)?;
                if !secs.is_finite() || secs < 0.0 {
                    return Err("--wait-source expects a number of seconds >= 0".into());
                }
                a.lost.wait_s = secs;
                a.wait_source_set = true;
            }
            "--exit-on-lost" => a.lost.exit_on_lost = true,
            "--allow-multiple" => a.allow_multiple = true,
            "--exit-after" => a.exit_after = Some(number("--exit-after", &value("--exit-after")?)?),
            "--log" => a.log = Some(value("--log")?),
            "--pattern" => a.pattern = Some(value("--pattern")?),
            "--pattern-size" => a.pattern_size = Some(patterns::parse_size(&value("--pattern-size")?)?),
            "--image" => a.image = Some(value("--image")?),
            "--preset" => a.preset = Some(value("--preset")?),
            "--crop-mode" => a.crop_mode = Some(value("--crop-mode")?),
            "--divisor-mode" => a.divisor_mode = Some(value("--divisor-mode")?),
            "--scale-mode" => a.scale_mode = Some(value("--scale-mode")?),
            "--scale" => {
                let n: i32 = number("--scale", &value("--scale")?)?;
                if !(0..=10).contains(&n) {
                    return Err("--scale expects 0 (automatic) or 1 to 10".into());
                }
                a.scale = Some(n);
            }
            "--align" => {
                a.center = Some(match value("--align")?.as_str() {
                    "center" | "centre" => true,
                    "top-left" | "top_left" => false,
                    other => return Err(format!("--align expects center or top-left, got \"{other}\"").into()),
                })
            }
            "--surround" => {
                let text = value("--surround")?;
                let v: Vec<f32> = text
                    .split(',')
                    .map(|p| number::<f32>("--surround", p.trim()))
                    .collect::<Res<_>>()?;
                match v[..] {
                    [r, g, b] if v.iter().all(|c| (0.0..=1.0).contains(c)) => a.surround = Some([r, g, b]),
                    _ => return Err("--surround expects r,g,b with values from 0 to 1".into()),
                }
            }
            "--source-colour" => a.colour = Some(parse_colour("--source-colour", &value("--source-colour")?)?),
            "--scan" => a.scan = Some(value("--scan")?),
            "--par" => a.par = Some(parse_ratio("--par", &value("--par")?)?),
            "--aspect" => {
                a.aspect = Some(match value("--aspect")?.as_str() {
                    "par" => Aspect::Par,
                    "fill" | "fill-tube" | "fill_tube" => Aspect::Fill,
                    other => Aspect::Display(parse_ratio("--aspect", other)?),
                })
            }
            "--version" => {
                println!("phoxium {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "-h" | "--help" => {
                println!("{USAGE}\n{}", hotkeys_help());
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument {other}").into()),
        }
    }
    if a.video.is_some() && (a.targeted() || a.pattern.is_some() || a.image.is_some()) {
        return Err("--video cannot be combined with --window, --process, --pattern or --image".into());
    }
    Ok(a)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::{
        RASTER_ASPECT_FILL_TUBE, SOURCE_CROP_MODE_MANUAL, SOURCE_CROP_MODE_NONE, SOURCE_DIVISOR_MODE_MANUAL,
        SOURCE_SCALE_MODE_FIT_FREE, SOURCE_SCALE_MODE_INTEGER,
    };

    fn parse(s: &str) -> Res<Args> {
        parse_from(s.split_whitespace().map(String::from))
    }

    /// `--scan <mode>`: the value of `scan_mode` by name, an unknown name is an error when applied.
    #[test]
    fn scan_flag() {
        let mut p = Params::default();
        assert_eq!(p.scan_mode, crate::params::SCAN_MODE_OFF);
        parse("").unwrap().apply_source(&mut p).unwrap();
        assert_eq!(p.scan_mode, crate::params::SCAN_MODE_OFF, "no option leaves it alone");
        parse("--scan rolling").unwrap().apply_source(&mut p).unwrap();
        assert_eq!(p.scan_mode, crate::params::SCAN_MODE_ROLLING);
        parse("--scan strobe").unwrap().apply_source(&mut p).unwrap();
        assert_eq!(p.scan_mode, crate::params::SCAN_MODE_STROBE);
        parse("--scan off").unwrap().apply_source(&mut p).unwrap();
        assert_eq!(p.scan_mode, crate::params::SCAN_MODE_OFF);
        let e = parse("--scan sideways")
            .unwrap()
            .apply_source(&mut p)
            .expect_err("refused");
        assert!(e.to_string().contains("--scan"), "{e}");
        assert!(parse("--scan").is_err());
    }

    /// `--video` and `--list-video`: parsed, and refused with any other way to name the source.
    #[test]
    fn video_flags() {
        let a = parse("--video USB").unwrap();
        assert_eq!(a.video.as_deref(), Some("USB"));
        assert!(!a.targeted() && !a.list_video);
        assert!(parse("--list-video").unwrap().list_video);
        assert!(parse("--video").is_err());
        for other in ["--window x", "--process p", "--pattern ramp", "--image a.png"] {
            let e = parse(&format!("--video v {other}")).err().expect("refused").to_string();
            assert!(e.contains("--video cannot be combined"), "{other}: {e}");
        }
    }

    /// `--process` and `--wait-window` (launcher automation): parsed, `targeted` covers both ways to name a window.
    #[test]
    fn process_and_wait_window_flags() {
        let a = parse("").unwrap();
        assert!(!a.targeted() && a.process.is_none() && a.wait_window.is_none());
        let a = parse("--process Game.exe --wait-window 30").unwrap();
        assert_eq!((a.process.as_deref(), a.wait_window), (Some("Game.exe"), Some(30.0)));
        assert!(a.targeted());
        assert!(a.target_text().contains("program \"Game.exe\""), "{}", a.target_text());
        let a = parse("--process game --window Level").unwrap();
        assert!(
            a.target_text().contains("\"Level\" in its title"),
            "{}",
            a.target_text()
        );
        assert!(parse("--window x").unwrap().targeted());
        assert!(parse("--wait-window -1").is_err());
        assert!(parse("--wait-window soon").is_err());
        assert!(parse("--process").is_err());
    }

    /// A29: `--capture-path` takes the two path names (`zero_copy` too), anything else is an error.
    #[test]
    fn capture_path_flag() {
        use crate::zerocopy::CapturePath;
        assert_eq!(parse("").unwrap().capture_path, None);
        assert_eq!(
            parse("--capture-path cpu").unwrap().capture_path,
            Some(CapturePath::Cpu)
        );
        assert_eq!(
            parse("--capture-path zero-copy").unwrap().capture_path,
            Some(CapturePath::ZeroCopy)
        );
        assert_eq!(
            parse("--capture-path zero_copy").unwrap().capture_path,
            Some(CapturePath::ZeroCopy)
        );
        // `auto` is the default: zero-copy with the automatic fallback.
        assert_eq!(
            parse("--capture-path auto").unwrap().capture_path,
            Some(CapturePath::default())
        );
        assert!(parse("--capture-path shared").is_err());
        assert!(parse("--capture-path").is_err());
        assert!(USAGE.contains("--capture-path <auto|zero-copy|cpu>"));
    }

    /// A23: `--wait-source` wins over the settings only when given; `--window` stays a developer option.
    #[test]
    fn wait_source_is_marked_when_given() {
        let a = parse("").unwrap();
        assert!(!a.wait_source_set);
        assert_eq!(a.window, None);
        let a = parse("--wait-source 5 --window pixel").unwrap();
        assert!(a.wait_source_set);
        assert_eq!(a.lost.wait_s, 5.0);
        assert_eq!(a.window.as_deref(), Some("pixel"));
        assert!(USAGE.contains("Without options"));
        // The hotkeys come from the action table: every action with its default chord, and no chord is written twice.
        let help = hotkeys_help();
        for a in crate::hotkeys::Action::ALL {
            assert!(help.contains(a.default_text()) && help.contains(a.describe()), "{a:?}");
        }
        assert!(
            !USAGE.contains("Ctrl+Alt+F9"),
            "the usage text holds no copy of a default chord"
        );
    }

    #[test]
    fn list_adapters_flag() {
        assert!(parse("--list-adapters").unwrap().list_adapters);
        assert!(parse("--adapters").unwrap().list_adapters);
        assert!(!parse("--list-gpus").unwrap().list_adapters);
    }

    #[test]
    fn detect_bars_takes_a_png() {
        assert_eq!(
            parse("--detect-bars frame.png").unwrap().detect_bars.as_deref(),
            Some("frame.png")
        );
        assert!(parse("--detect-bars").is_err());
        assert!(USAGE.contains("--detect-bars <png>"));
    }

    /// T-96: the source options parse and override the parameters; --crop and --divisor imply manual.
    #[test]
    fn source_options_apply_to_the_parameters() {
        let a =
            parse("--scale-mode integer --scale 3 --align top-left --surround 0.5,0,1 --par 8:7 --aspect 4:3").unwrap();
        let mut p = Params::default();
        a.apply_source(&mut p).unwrap();
        assert_eq!((p.source_scale_mode, p.source_scale), (SOURCE_SCALE_MODE_INTEGER, 3));
        assert!(!p.source_center);
        assert_eq!(p.source_surround_color, [0.5, 0.0, 1.0]);
        assert_eq!(p.source_par, 8.0 / 7.0);
        assert_eq!(p.source_display_aspect, 4.0 / 3.0);
        let a = parse("--crop 0,0,320,240 --divisor 2x3 --scale-mode fit-free --aspect fill").unwrap();
        let mut p = Params::default();
        a.apply_source(&mut p).unwrap();
        assert_eq!(p.source_crop_mode, SOURCE_CROP_MODE_MANUAL);
        assert_eq!(p.source_divisor_mode, SOURCE_DIVISOR_MODE_MANUAL);
        assert_eq!(p.source_scale_mode, SOURCE_SCALE_MODE_FIT_FREE);
        assert_eq!(p.raster_aspect, RASTER_ASPECT_FILL_TUBE);
        let mut p = Params::default();
        parse("--no-crop").unwrap().apply_source(&mut p).unwrap();
        assert_eq!(p.source_crop_mode, SOURCE_CROP_MODE_NONE);
        // No option: the parameters are untouched.
        let mut p = Params::default();
        parse("").unwrap().apply_source(&mut p).unwrap();
        assert_eq!(p, Params::default());
        assert!(parse("--scale 11").is_err());
        assert!(parse("--surround 1,2").is_err());
        assert!(parse("--par 0:1").is_err());
        assert!(parse("--scale-mode huge").unwrap().apply_source(&mut p).is_err());
    }

    fn colour(text: &str) -> Res<ColourChoice> {
        // One argument, so that a variant name with spaces ("Kega Fusion") stays whole, as a quoted shell word does.
        Ok(parse_from(["--source-colour".to_string(), text.to_string()])?
            .colour
            .unwrap())
    }

    /// `--source-colour <target[:variant]>`: the target by its value name, the variant by number, id or name, the
    /// first variant when none is given; it sets the two parameters as `--set` would and wins over the profile.
    #[test]
    fn source_colour_flag() {
        use crate::params::{
            SOURCE_COLOUR_TARGET_NINTENDO_GAME_BOY_COLOR, SOURCE_COLOUR_TARGET_NINTENDO_SUPER_GAME_BOY,
            SOURCE_COLOUR_TARGET_NONE,
        };
        assert_eq!(parse("").unwrap().colour, None);
        let gbc = ColourChoice {
            target: SOURCE_COLOUR_TARGET_NINTENDO_GAME_BOY_COLOR,
            variant: 1,
        };
        assert_eq!(colour("nintendo_game_boy_color").unwrap(), gbc);
        // Case, dashes and underscores do not matter, as for the other value names.
        assert_eq!(colour("Nintendo-Game-Boy-Color").unwrap(), gbc);
        // The ids of the builds before the provider-first names are no longer accepted (no migration).
        assert!(colour("game_boy_color").is_err());
        assert_eq!(colour("none").unwrap().target, SOURCE_COLOUR_TARGET_NONE);
        // Variants of the Super Game Boy: a number, an id, a name, any case, dash or underscore.
        let sgb = |variant| ColourChoice {
            target: SOURCE_COLOUR_TARGET_NINTENDO_SUPER_GAME_BOY,
            variant,
        };
        assert_eq!(colour("nintendo_super_game_boy").unwrap(), sgb(1));
        assert_eq!(colour("nintendo_super_game_boy:1").unwrap(), sgb(1));
        assert_eq!(colour("nintendo_super_game_boy:9").unwrap(), sgb(9));
        assert_eq!(colour("nintendo_super_game_boy:32").unwrap(), sgb(32));
        assert_eq!(colour("nintendo_super_game_boy:2-a").unwrap(), sgb(9));
        assert_eq!(colour("nintendo_super_game_boy:2_A").unwrap(), sgb(9));
        assert_eq!(colour("nintendo-super-game-boy:4-h").unwrap(), sgb(32));
        // A variant by the name the menu shows (spaces read as `_`).
        let md = colour("sega_mega_drive:Kega Fusion").unwrap();
        assert_eq!(md.variant, 2);
        assert_eq!(colour("sega_mega_drive:kega_fusion").unwrap(), md);
        assert_eq!(colour("sega_mega_drive:regen").unwrap().variant, 3);
        // An id made of digits is reached when it is not a position: the Atari 800 (3rd) and 400 (4th) by their ids,
        // the 3rd by its position too.
        assert_eq!(colour("atari_8bit_gtia:800").unwrap().variant, 3);
        assert_eq!(colour("atari_8bit_gtia:400").unwrap().variant, 4);
        assert_eq!(colour("atari_8bit_gtia:3").unwrap().variant, 3);
        assert_eq!(colour("atari_2600_tia:24").unwrap().variant, 2);
        // Errors name the option and what is accepted.
        let e = colour("gameboy").unwrap_err().to_string();
        assert!(
            e.starts_with("--source-colour expects <target>[:<variant>]")
                && e.contains("amstrad_cpc, amstrad_cpc_green_screen")
                && e.contains(
                    "nintendo_game_boy_advance, nintendo_game_boy_advance_sp_ags_101, nintendo_game_boy_color"
                ),
            "{e}"
        );
        assert!(e.ends_with("got \"gameboy\""), "{e}");
        for (text, wanted) in [
            ("nintendo_super_game_boy:0", "a number from 1 to 32"),
            ("nintendo_super_game_boy:33", "1_a, 1_b"),
            ("nintendo_super_game_boy:9-z", "has no variant \"9-z\""),
            ("nintendo_super_game_boy:", "has no variant \"\""),
            ("sega_mega_drive:snes", "genesis_plus_gx, kega_fusion, regen"),
            ("none:1", "\"none\" has no variants"),
            ("nintendo_game_boy_color:1", "has no variants"),
        ] {
            let e = colour(text).unwrap_err().to_string();
            assert!(e.starts_with("--source-colour: ") && e.contains(wanted), "{text}: {e}");
        }
        assert!(parse("--source-colour").is_err());
        assert!(USAGE.contains("--source-colour <target[:variant]>"));
        // It sets both parameters as `--set` would, over what a profile put there.
        let mut p = Params::default();
        p.set("source_colour_variant", ParamValue::I32(17));
        parse("--source-colour nintendo_super_game_boy:2-a")
            .unwrap()
            .apply_source(&mut p)
            .unwrap();
        assert_eq!(
            (p.source_colour_target, p.source_colour_variant),
            (SOURCE_COLOUR_TARGET_NINTENDO_SUPER_GAME_BOY, 9)
        );
        // Without a variant the first is selected, whatever the variant was.
        parse("--source-colour nintendo_game_boy_color")
            .unwrap()
            .apply_source(&mut p)
            .unwrap();
        assert_eq!(
            (p.source_colour_target, p.source_colour_variant),
            (SOURCE_COLOUR_TARGET_NINTENDO_GAME_BOY_COLOR, 1)
        );
        parse("--source-colour none").unwrap().apply_source(&mut p).unwrap();
        assert_eq!(p.source_colour_target, SOURCE_COLOUR_TARGET_NONE);
        // The same result as the generic path (`render --set`) with the same values.
        let mut q = Params::default();
        crate::render_cmd::set_param(&mut q, "source_colour_target", "nintendo_super_game_boy").unwrap();
        crate::render_cmd::set_param(&mut q, "source_colour_variant", "9").unwrap();
        let mut p = Params::default();
        parse("--source-colour nintendo_super_game_boy:9")
            .unwrap()
            .apply_source(&mut p)
            .unwrap();
        assert_eq!(p, q);
    }

    /// The variant text of every shipped target is unambiguous: every variant is found by its number, its id and its
    /// name; no id is made of digits only (that is a number); no two variants share an id or a name.
    #[test]
    fn every_shipped_variant_is_reachable_by_number_id_and_name() {
        for t in crate::targets::shipped() {
            for (i, v) in t.variants.iter().enumerate() {
                // An id made of digits must not be a position (it would be shadowed by the number).
                if v.id.bytes().all(|b| b.is_ascii_digit()) {
                    let n: usize = v.id.parse().unwrap();
                    assert!(
                        !(1..=t.variants.len()).contains(&n),
                        "{}: id {} is a position",
                        t.id,
                        v.id
                    );
                }
                for text in [(i + 1).to_string(), v.id.clone(), v.name.clone()] {
                    assert_eq!(variant_by_text(t, &text), Some(i), "{}: \"{text}\"", t.id);
                    let c = parse_colour("--source-colour", &format!("{}:{text}", t.id)).unwrap();
                    assert_eq!(c.variant, i as i32 + 1, "{}: \"{text}\"", t.id);
                }
            }
            assert_eq!(
                variant_by_text(t, &(t.variants.len() + 1).to_string()),
                None,
                "{}",
                t.id
            );
        }
    }

    /// Review D-07: --par, --aspect and --surround go through the checked setter. Values outside the hard
    /// range (or under the forced-aspect minimum) are clamped with a notice that names the option; values
    /// inside pass exactly with no notice.
    #[test]
    fn source_options_respect_the_hard_ranges() {
        use crate::params::{self, MIN_DISPLAY_ASPECT};
        params::clear_notices();
        let mut p = Params::default();
        parse("--par 1e-6 --aspect 0.01").unwrap().apply_source(&mut p).unwrap();
        let par_min = params::meta("source_par").unwrap().hard_range.unwrap().0 as f32;
        assert_eq!(p.source_par, par_min);
        assert_eq!(p.source_display_aspect, MIN_DISPLAY_ASPECT);
        let n = params::notices();
        assert!(
            n.iter().any(|m| m.starts_with("--par: ") && m.contains("source_par")),
            "{n:?}"
        );
        assert!(
            n.iter().any(|m| m.starts_with("--aspect: ") && m.contains("0.25")),
            "{n:?}"
        );
        params::clear_notices();
        let mut p = Params::default();
        parse("--par 1000 --aspect 1000:1")
            .unwrap()
            .apply_source(&mut p)
            .unwrap();
        let (par_max, aspect_max) = (
            params::meta("source_par").unwrap().hard_range.unwrap().1 as f32,
            params::meta("source_display_aspect").unwrap().hard_range.unwrap().1 as f32,
        );
        assert_eq!((p.source_par, p.source_display_aspect), (par_max, aspect_max));
        assert_eq!(params::notices().len(), 2, "{:?}", params::notices());
        params::clear_notices();
        let mut p = Params::default();
        parse("--par 8:7 --aspect 4:3 --surround 0.5,0,1")
            .unwrap()
            .apply_source(&mut p)
            .unwrap();
        assert_eq!(
            (p.source_par, p.source_display_aspect, p.source_surround_color),
            (8.0 / 7.0, 4.0 / 3.0, [0.5, 0.0, 1.0])
        );
        assert!(params::notices().is_empty(), "{:?}", params::notices());
        // A value the setter rejects is an error naming the option.
        let e = set_flag(&mut p, "--par", "source_par", ParamValue::F32(f32::NAN)).unwrap_err();
        assert!(
            e.to_string().starts_with("--par: ") && e.to_string().contains("rejected"),
            "{e}"
        );
        assert_eq!(p.source_par, 8.0 / 7.0, "a rejected value changes nothing");
    }
}
