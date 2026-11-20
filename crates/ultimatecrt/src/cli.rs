//! Command line: the usage text, the parsed options and the parser.

use std::env;

use crate::{lifecycle, params::Params, patterns, scaling, Res};

pub const USAGE: &str = "ultimatecrt: captures a window and shows it in a second window through a GPU pipeline.

Without options the output window opens and lists the windows to capture: choose one there (Enter or
double-click). Change source and Detach are in the settings menu (F10). The options below are developer and
diagnostic conveniences.

Usage: ultimatecrt [options]
  --window <text>      capture the window whose title starts with <text>, else contains it, at once
                       (no picker; an error when there is none)
  --list-windows       list capturable windows and monitors, then exit
  --list-gpus          list Vulkan devices, then exit
  --list-adapters      adapter topology (DXGI adapters and monitors, Vulkan devices, LUIDs, the chosen adapter,
                       capture adapter, verdict, warnings), then exit; opens no window
  --gpu <index>        Vulkan device (see --list-gpus) that runs the chain, capture included; wins over the
                       settings file's [global.gpu] adapter (default auto: the adapter driving the monitor the
                       output covers)
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
  --scale-mode <tube|native|integer|fit|fit-free>  picture size: by the tube (default, physical), 1:1 host
                       pixels per source pixel, xN (--scale), the largest whole factor that fits, or the
                       largest non-integer fit (breaks the scanline and mask alignment)
  --scale <N>          host pixels per source row in the integer mode: 0 = largest that fits (default),
                       1 to 10 manual
  --align <center|top-left>  picture position in the output (default center)
  --surround <r,g,b>   colour outside the picture, output code values 0 to 1 (default 0,0,0: black)
  --par <W:H|number>   pixel aspect ratio of a source pixel (1:1 default, 8:7 NES/SNES, 32:35 Genesis H40)
  --aspect <par|fill|W:H|number>  picture shape: from the PAR (default), the whole tube (fill), or a forced
                       display aspect such as 4:3
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
  --detect-bars <png>  offline: run the black-bar (picture area) detector on a saved capture (a --save-capture
                       PNG) with the settings' thresholds, print the picture found, the bars, the reason when
                       nothing is cropped and the per-edge statistics, then exit; opens no window
  --wait-source <secs> when the captured window closes: reattach to a new window of the same program and
                       class for this many seconds, then show the window list again (a fullscreen output
                       becomes a normal window). Default: [global.source] wait_s of the settings (30);
                       0 = wait forever
  --exit-on-lost       exit when the captured window closes instead of waiting for it
  --exit-after <secs>  quit after some seconds (for automated tests)
  --reset-burnin       start the burn-in wear map of the tube preset as a new tube (the saved wear is replaced
                       at the next save); the same as the menu button \"New tube\"
  render ...           (first argument) offline render of a pattern or image to PNG or PFM, no window; see `ultimatecrt render --help`
  --version            print the version and exit
  --help               print this text

Quit: Ctrl+Alt+Q (the output does not take the keyboard focus), or the close button of the framed window.
Hotkeys (global, configurable in settings.toml [global.hotkeys]): F11 fullscreen or framed window, F10
settings menu, Ctrl+Alt+O overlay, Ctrl+Alt+B A/B bypass of the CRT chain, Ctrl+Alt+PageDown and
Ctrl+Alt+PageUp next and previous preset. A chord another program owns falls back and is reported.";

#[derive(Default)]
pub struct Args {
    pub window: Option<String>,
    pub list_windows: bool,
    pub list_gpus: bool,
    pub list_adapters: bool,
    pub gpu: Option<usize>,
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
    /// `--detect-bars <png>`: the offline picture-area diagnostic.
    pub detect_bars: Option<String>,
    pub exit_after: Option<f64>,
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
        ] {
            if let Some(v) = value {
                if !set_enum_by_name(params, id, v) {
                    return Err(format!("{flag}: unknown value \"{v}\"").into());
                }
            }
        }
        if let Some(n) = self.scale {
            params.source_scale = n;
        }
        if let Some(c) = self.center {
            params.source_center = c;
        }
        if let Some(c) = self.surround {
            params.source_surround_color = c;
        }
        if let Some(p) = self.par {
            params.source_par = p;
        }
        match self.aspect {
            None => {}
            Some(Aspect::Par) => {
                params.raster_aspect = crate::params::RASTER_ASPECT_PAR;
                params.source_display_aspect = 0.0;
            }
            Some(Aspect::Fill) => params.raster_aspect = crate::params::RASTER_ASPECT_FILL_TUBE,
            Some(Aspect::Display(a)) => {
                params.raster_aspect = crate::params::RASTER_ASPECT_PAR;
                params.source_display_aspect = a;
            }
        }
        Ok(())
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
            "--list-windows" => a.list_windows = true,
            "--list-gpus" => a.list_gpus = true,
            "--list-adapters" | "--adapters" => a.list_adapters = true,
            "--gpu" => a.gpu = Some(number("--gpu", &value("--gpu")?)?),
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
            "--par" => a.par = Some(parse_ratio("--par", &value("--par")?)?),
            "--aspect" => {
                a.aspect = Some(match value("--aspect")?.as_str() {
                    "par" => Aspect::Par,
                    "fill" | "fill-tube" | "fill_tube" => Aspect::Fill,
                    other => Aspect::Display(parse_ratio("--aspect", other)?),
                })
            }
            "--version" => {
                println!("ultimatecrt {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument {other}").into()),
        }
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
}
