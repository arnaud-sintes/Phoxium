//! Command line: the usage text, the parsed options and the parser.

use std::env;

use crate::{lifecycle, patterns, scaling, Res};

pub const USAGE: &str = "ultimatecrt: captures a window and shows it in a second window through a GPU pipeline.

Usage: ultimatecrt [options]
  --window <text>      capture the window whose title starts with <text>, else contains it
                       (default \"pixel-demo\")
  --list-windows       list capturable windows and monitors, then exit
  --list-gpus          list Vulkan devices, then exit
  --gpu <index>        Vulkan device to use (default: NVIDIA discrete, else any discrete)
  --fullscreen         borderless, topmost, non-activating output covering the source's monitor
  --monitor <index>    monitor to cover with --fullscreen (see --list-windows; default: the source's)
  --no-activate        windowed output that never takes focus (Esc cannot reach it)
  --no-crop            capture the whole window instead of its client area
  --crop <x,y,w,h>     part of the captured client area that is the source (default: all of it)
  --divisor <N|NxM>    the source is an integer upscale by N (or N horizontally, M vertically);
                       1 means native. Default: detected from the first informative frames
  --integer            integer scale only (letterboxed)
  --pattern <name[:args]>  generated test pattern instead of a capture (no window needed): flat[:level],
                       ramp, vline[:N], hline[:N], grating[:N], checker[:N], crosshatch[:N], point,
                       window[:APL%], bar[:width[:px-per-frame]], pluge, moire[:N]
  --pattern-size <WxH> size of the pattern (default 320x240)
  --image <file.png>   still PNG image instead of a capture (divisor 1)
  --preset <id|file.toml>  tube preset to start with: a shipped or user preset id, or a preset file (default:
                       the preset of the settings file, else the parameter table defaults); the parameter
                       overrides of the settings file apply on top
  --validation         enable the Vulkan validation layer (needs the Vulkan SDK)
  --stats             print capture and render rates every second
  --log <file.csv>     write a diagnostics CSV: one row per second (capture and render fps, min/max/p99 intervals,
                       gaps over 50 ms, foreground window changes) and a summary at exit
  --save-capture <png>save the first captured frame and exit
  --wait-source <secs> when the captured window closes: a fullscreen output drops to a normal window after
                       this many seconds (default 30; 0 = wait forever). Reattaches to a new window of the
                       same program and class meanwhile
  --exit-on-lost       exit when the captured window closes instead of waiting for it
  --exit-after <secs>  quit after some seconds (for automated tests)
  render ...           (first argument) offline render of a pattern or image to PNG or PFM, no window; see `ultimatecrt render --help`
  --version            print the version and exit
  --help               print this text

Quit: Ctrl+Alt+Q (the output cannot take keyboard focus in fullscreen), or Esc in an activating
windowed output.";

#[derive(Default)]
pub struct Args {
    pub window: Option<String>,
    pub list_windows: bool,
    pub list_gpus: bool,
    pub gpu: Option<usize>,
    pub fullscreen: bool,
    pub monitor: Option<usize>,
    pub no_activate: bool,
    pub no_crop: bool,
    pub crop: Option<scaling::Rect>,
    pub divisor: Option<(u32, u32)>,
    pub integer: bool,
    pub validation: bool,
    pub stats: bool,
    pub save_capture: Option<String>,
    pub exit_after: Option<f64>,
    pub lost: lifecycle::LostPolicy,
    pub log: Option<String>,
    pub pattern: Option<String>,
    pub pattern_size: Option<(u32, u32)>,
    pub image: Option<String>,
    pub preset: Option<String>,
}

fn number<T: std::str::FromStr>(flag: &str, text: &str) -> Res<T> {
    text.parse()
        .map_err(|_| format!("{flag} expects a number, got \"{text}\"").into())
}

pub fn parse_args() -> Res<Args> {
    let mut a = Args::default();
    let mut it = env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = |name: &str| it.next().ok_or_else(|| format!("{name} expects a value"));
        match arg.as_str() {
            "--window" => a.window = Some(value("--window")?),
            "--list-windows" => a.list_windows = true,
            "--list-gpus" => a.list_gpus = true,
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
            "--save-capture" => a.save_capture = Some(value("--save-capture")?),
            "--wait-source" => {
                let secs: f64 = number("--wait-source", &value("--wait-source")?)?;
                if !secs.is_finite() || secs < 0.0 {
                    return Err("--wait-source expects a number of seconds >= 0".into());
                }
                a.lost.wait_s = secs;
            }
            "--exit-on-lost" => a.lost.exit_on_lost = true,
            "--exit-after" => a.exit_after = Some(number("--exit-after", &value("--exit-after")?)?),
            "--log" => a.log = Some(value("--log")?),
            "--pattern" => a.pattern = Some(value("--pattern")?),
            "--pattern-size" => a.pattern_size = Some(patterns::parse_size(&value("--pattern-size")?)?),
            "--image" => a.image = Some(value("--image")?),
            "--preset" => a.preset = Some(value("--preset")?),
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
