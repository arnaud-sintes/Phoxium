//! Command line parsing, pure and unit tested. Every invalid combination is rejected with a
//! message that names the flags involved.

use crate::{
    layout::{Align, Par, ScaleMode, MAX_SCALE},
    patterns::BarPattern,
    resample::Filter,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Content {
    Scene,
    Card,
    Grid,
    Bars,
    Ramp,
    Checker,
    Motion,
    Text,
    Noise,
    Image,
}

/// Native size presets of `--native-preset` and the N key, in cycling order.
pub const NATIVE_PRESETS: [(&str, (usize, usize)); 7] = [
    ("nes", (256, 240)),
    ("snes", (256, 224)),
    ("gb", (160, 144)),
    ("dos", (320, 200)),
    ("vga", (640, 480)),
    ("hd720", (1280, 720)),
    ("hd1080", (1920, 1080)),
];

/// Next native preset after `size` (the first one when `size` is not a preset).
pub fn next_native_preset(size: (usize, usize)) -> (&'static str, (usize, usize)) {
    let i = NATIVE_PRESETS.iter().position(|(_, s)| *s == size);
    NATIVE_PRESETS[i.map_or(0, |i| (i + 1) % NATIVE_PRESETS.len())]
}

/// Bar colours of `--bars NAME` and the B key, in cycling order.
pub const BAR_COLORS: [(&str, u32); 7] = [
    ("black", 0x000000),
    ("grey", 0x202020),
    ("white", 0xFFFFFF),
    ("red", 0x800000),
    ("green", 0x008000),
    ("blue", 0x000080),
    ("magenta", 0xFF00FF),
];

/// Next bar colour after `color` (black when `color` is not in the list).
pub fn next_bar_color(color: u32) -> (&'static str, u32) {
    let i = BAR_COLORS.iter().position(|(_, c)| *c == color);
    BAR_COLORS[i.map_or(0, |i| (i + 1) % BAR_COLORS.len())]
}

/// Name of a bar colour: its list name, else `#rrggbb`.
pub fn bar_color_name(color: u32) -> String {
    BAR_COLORS
        .iter()
        .find(|(_, c)| *c == color)
        .map_or_else(|| format!("#{color:06x}"), |(n, _)| n.to_string())
}

impl Content {
    pub const ALL: [Content; 10] = [
        Content::Scene,
        Content::Card,
        Content::Grid,
        Content::Bars,
        Content::Ramp,
        Content::Checker,
        Content::Motion,
        Content::Text,
        Content::Noise,
        Content::Image,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Content::Scene => "scene",
            Content::Card => "card",
            Content::Grid => "grid",
            Content::Bars => "bars",
            Content::Ramp => "ramp",
            Content::Checker => "checker",
            Content::Motion => "motion",
            Content::Text => "text",
            Content::Noise => "noise",
            Content::Image => "image",
        }
    }

    /// Parses a content name or an alias (`testcard` = `card`, `gradient` = `ramp`).
    pub fn parse(s: &str) -> Option<Content> {
        match s {
            "testcard" => Some(Content::Card),
            "gradient" => Some(Content::Ramp),
            _ => Content::ALL.into_iter().find(|c| c.name() == s),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    /// Native image size.
    pub native: (usize, usize),
    /// True when `--native` or `--res` was given (otherwise `--image` sets the size).
    pub native_given: bool,
    pub mode: ScaleMode,
    pub filter: Filter,
    pub align: Align,
    /// Bar colour, 0x00RRGGBB.
    pub bg: u32,
    /// Pattern drawn in the bars (with `bg` and its contrast colour), `None` for plain bars.
    pub bars_pattern: BarPattern,
    pub par: Par,
    pub content: Content,
    pub image: Option<String>,
    pub fps: u32,
    pub fixed_seed: bool,
    /// `Some(true)` forces the frame counter on, `Some(false)` off, `None` is on for `motion` only.
    pub counter: Option<bool>,
    /// `Some(true)` forces the corner markers on, `Some(false)` off, `None` is on for `grid` and
    /// `motion` only.
    pub markers: Option<bool>,
    pub borderless: bool,
    /// Not resizable by the user (no thick frame, no maximise box).
    pub fixed_size: bool,
    pub maximised: bool,
    pub fullscreen: bool,
    pub no_round_corners: bool,
    /// Initial client size; the default is the content size at the scale.
    pub window_size: Option<(u32, u32)>,
    pub exit_after: Option<f64>,
    pub stats: bool,
    /// Offline decode of the frame counter from a PNG capture of the client area.
    pub decode: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            native: (256, 240),
            native_given: false,
            mode: ScaleMode::Integer(3),
            filter: Filter::Nearest,
            align: Align::Center,
            bg: 0,
            bars_pattern: BarPattern::None,
            par: Par::SQUARE,
            content: Content::Scene,
            image: None,
            fps: 60,
            fixed_seed: false,
            counter: None,
            markers: None,
            borderless: false,
            fixed_size: false,
            maximised: false,
            fullscreen: false,
            no_round_corners: false,
            window_size: None,
            exit_after: None,
            stats: false,
            decode: None,
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum Parsed {
    Run(Box<Options>),
    Help,
    Version,
}

pub const HELP: &str = "pixel-demo: universal test source for ultimatecrt (emulates how games present a
low-resolution surface inside their window)
usage: pixel-demo [options]

emulated game surface
  --native WxH        native surface size, 16..8192 each (default 256x240; alias --res),
                      e.g. 256x240, 320x200, 640x480, 1280x720, 1920x1080
  --native-preset P   nes 256x240 | snes 256x224 | gb 160x144 | dos 320x200 | vga 640x480 |
                      hd720 1280x720 | hd1080 1920x1080 (exclusive with --native)
  --par N:M | X       pixel aspect ratio, width:height of one pixel, N and M 1..1000 (e.g. 8:7),
                      or a decimal 0.25..4 (e.g. 1.2); ignored by the 1:1 presentation

presentation of the surface inside the client area
  --present P         1to1     true 1:1 pixels (the default client is the native size)
                      integer  integer scale --scale N (nearest)
                      autofit  largest integer scale 1..10 that fits the client (nearest)
                      fit      largest non-integer fit, aspect kept (--filter applies)
                      stretch  fills the client, aspect not kept (--filter applies)
  --mode M            same, older names: native | integer | fit (= autofit) | fit-free (= fit)
                      | stretch; default integer x3. --present and --mode are exclusive
  --scale N           integer scale 1..10 (integer only, default 3)
  --filter F          nearest | bilinear | bicubic (fit and stretch only, default nearest)
  --align A           center | top-left (default center: centred, offsets floored)
  --bars C            bar colour around the picture: black | grey | white | red | green | blue |
                      magenta | #rrggbb (default black; alias --bg)
  --bars-pattern P    none | checker | stripes | noise: fill the bars with the bar colour and a
                      second colour 64 codes away (tests surround detection; default none)

window (client sizes are physical pixels: the process is per-monitor DPI aware v2, so 125 %
or 150 % scaling does not change them; a move to a monitor with another scaling keeps an
exact-size window exact)
  --window WxH        initial client size 16..16384 (alias --window-size; default: the
                      picture at its scale, x3 for autofit, fit and stretch)
  --fixed-size        the user cannot resize the window (no thick frame, no maximise box);
                      otherwise it is resizable and re-lays out the picture live
  --borderless        popup window without frame
  --maximised         start maximised
  --fullscreen        start fullscreen
  --no-round-corners  ask Windows 11 not to round the window corners

content
  --content C         scene | card (alias testcard) | grid | bars | ramp (alias gradient) |
                      checker | motion | text | noise | image (alias --scene; default scene)
  --image file.png    PNG source (content image; sets the native size unless --native is given)
  --fps N             update rate 1..240 (default 60), driven by a high-resolution timer
  --fixed-seed        deterministic noise (same pixels for the same frame number)
  --counter / --no-counter   binary frame counter in the top-left corner (default: motion only)
  --markers / --no-markers   red, green, blue, yellow squares in the four corners of the surface
                      (default: grid and motion only)

run
  --exit-after SECS   quit by itself
  --stats             print the achieved update rate every second
  --decode file.png   offline: print the frame counter of a client-area capture, using the
                      layout options above (present/mode, scale, native, par, align); exit code 1
                      if absent
  --version, -h/--help

keys (when the window has the focus; the title lists the state and the keys)
  1-9, 0   integer scale x1..x9, x10      M  cycle presentation 1to1, integer, autofit, fit,
  F        cycle filter                      stretch
  B        cycle bar colour               P  cycle bar pattern
  C, Space cycle content                  N  cycle native preset (resizes an exact window)
  K        frame counter on/off           X  corner markers on/off
  A        alignment                      Alt+Enter fullscreen (F11 is ultimatecrt's)
  Esc      quit";

fn parse_wxh(flag: &str, v: &str, min: u32, max: u32) -> Result<(u32, u32), String> {
    let (a, b) = v
        .split_once(['x', 'X'])
        .ok_or_else(|| format!("{flag} expects WxH, got '{v}'"))?;
    match (a.parse::<u32>(), b.parse::<u32>()) {
        (Ok(a), Ok(b)) if (min..=max).contains(&a) && (min..=max).contains(&b) => Ok((a, b)),
        (Ok(_), Ok(_)) => Err(format!("{flag} sizes must be between {min} and {max}, got '{v}'")),
        _ => Err(format!("{flag} expects WxH, got '{v}'")),
    }
}

/// A bar colour: a name of `BAR_COLORS` or `#rrggbb` (the `#` is optional).
pub fn parse_color(flag: &str, v: &str) -> Result<u32, String> {
    if let Some((_, c)) = BAR_COLORS.iter().find(|(n, _)| *n == v.to_ascii_lowercase()) {
        return Ok(*c);
    }
    let h = v.strip_prefix('#').unwrap_or(v);
    if h.len() == 6 && h.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(u32::from_str_radix(h, 16).unwrap())
    } else {
        Err(format!(
            "{flag} expects black, grey, white, red, green, blue, magenta or #rrggbb, got '{v}'"
        ))
    }
}

/// `N:M` (both 1..1000) or a decimal 0.25..4 (kept to 1/1000).
fn parse_par(v: &str) -> Result<Par, String> {
    if let Some((a, b)) = v.split_once(':') {
        return match (a.parse::<u32>(), b.parse::<u32>()) {
            (Ok(n), Ok(m)) if (1..=1000).contains(&n) && (1..=1000).contains(&m) => Ok(Par { n, m }),
            _ => Err(format!("--par expects N:M with both 1..1000, got '{v}'")),
        };
    }
    match v.parse::<f64>() {
        Ok(x) if x.is_finite() && (0.25..=4.0).contains(&x) => Ok(Par {
            n: (x * 1000.0).round() as u32,
            m: 1000,
        }),
        _ => Err(format!("--par expects N:M or a decimal 0.25..4, got '{v}'")),
    }
}

/// Parses the arguments after the program name.
pub fn parse(args: &[String]) -> Result<Parsed, String> {
    let mut o = Options::default();
    // Mode name in the --mode vocabulary, and the flag that gave it.
    let mut mode_name: Option<String> = None;
    let mut mode_flag: Option<&str> = None;
    let mut preset_given = false;
    let mut scale: Option<u32> = None;
    let mut filter_given = false;
    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        let mut value = || -> Result<String, String> {
            i += 1;
            args.get(i).cloned().ok_or_else(|| format!("{flag} expects a value"))
        };
        match flag {
            "--native" | "--res" => {
                let (w, h) = parse_wxh(flag, &value()?, 16, 8192)?;
                o.native = (w as usize, h as usize);
                o.native_given = true;
            }
            "--native-preset" => {
                let v = value()?;
                o.native = NATIVE_PRESETS
                    .iter()
                    .find(|(n, _)| *n == v)
                    .map(|(_, s)| *s)
                    .ok_or_else(|| {
                        format!("--native-preset expects nes, snes, gb, dos, vga, hd720 or hd1080, got '{v}'")
                    })?;
                preset_given = true;
            }
            "--mode" | "--present" => {
                if let Some(prev) = mode_flag {
                    if prev != flag {
                        return Err("--present and --mode are exclusive (use one)".into());
                    }
                }
                let v = value()?;
                let name = if flag == "--present" {
                    match v.as_str() {
                        "1to1" => "native",
                        "integer" => "integer",
                        "autofit" => "fit",
                        "fit" => "fit-free",
                        "stretch" => "stretch",
                        _ => {
                            return Err(format!(
                                "--present expects 1to1, integer, autofit, fit or stretch, got '{v}'"
                            ))
                        }
                    }
                    .to_string()
                } else {
                    v
                };
                mode_name = Some(name);
                mode_flag = Some(flag);
            }
            "--scale" => {
                let v = value()?;
                scale = Some(
                    v.parse::<u32>()
                        .ok()
                        .filter(|s| (1..=MAX_SCALE).contains(s))
                        .ok_or_else(|| format!("--scale expects 1 to {MAX_SCALE}, got '{v}'"))?,
                );
            }
            "--filter" => {
                let v = value()?;
                o.filter = match v.as_str() {
                    "nearest" => Filter::Nearest,
                    "bilinear" => Filter::Bilinear,
                    "bicubic" => Filter::Bicubic,
                    _ => return Err(format!("--filter expects nearest, bilinear or bicubic, got '{v}'")),
                };
                filter_given = true;
            }
            "--align" => {
                let v = value()?;
                o.align = match v.as_str() {
                    "center" | "centre" => Align::Center,
                    "top-left" => Align::TopLeft,
                    _ => return Err(format!("--align expects center or top-left, got '{v}'")),
                };
            }
            "--bg" | "--bars" => o.bg = parse_color(flag, &value()?)?,
            "--bars-pattern" => {
                let v = value()?;
                o.bars_pattern = BarPattern::parse(&v)
                    .ok_or_else(|| format!("--bars-pattern expects none, checker, stripes or noise, got '{v}'"))?;
            }
            "--par" => o.par = parse_par(&value()?)?,
            "--content" | "--scene" => {
                let v = value()?;
                o.content = Content::parse(&v).ok_or_else(|| {
                    format!(
                        "{flag} expects scene, card, testcard, grid, bars, ramp, gradient, checker, motion, text, \
                         noise or image, got '{v}'"
                    )
                })?;
            }
            "--image" => o.image = Some(value()?),
            "--fps" => {
                let v = value()?;
                o.fps = v
                    .parse::<u32>()
                    .ok()
                    .filter(|f| (1..=240).contains(f))
                    .ok_or_else(|| format!("--fps expects 1 to 240, got '{v}'"))?;
            }
            "--fixed-seed" => o.fixed_seed = true,
            "--counter" => o.counter = Some(true),
            "--no-counter" => o.counter = Some(false),
            "--markers" => o.markers = Some(true),
            "--no-markers" => o.markers = Some(false),
            "--borderless" => o.borderless = true,
            "--fixed-size" => o.fixed_size = true,
            "--maximised" | "--maximized" => o.maximised = true,
            "--fullscreen" => o.fullscreen = true,
            "--no-round-corners" => o.no_round_corners = true,
            "--window-size" | "--window" => {
                let (w, h) = parse_wxh(flag, &value()?, 16, 16384)?;
                o.window_size = Some((w, h));
            }
            "--exit-after" => {
                let v = value()?;
                o.exit_after = Some(
                    v.parse::<f64>()
                        .ok()
                        .filter(|s| s.is_finite() && *s > 0.0)
                        .ok_or_else(|| format!("--exit-after expects a positive number of seconds, got '{v}'"))?,
                );
            }
            "--stats" => o.stats = true,
            "--decode" => o.decode = Some(value()?),
            "--version" => return Ok(Parsed::Version),
            "-h" | "--help" => return Ok(Parsed::Help),
            other => return Err(format!("unknown argument {other}")),
        }
        i += 1;
    }

    o.mode = match (mode_name.as_deref(), scale) {
        (None, None) => ScaleMode::Integer(3),
        (None, Some(n)) | (Some("integer"), Some(n)) => ScaleMode::Integer(n),
        (Some("integer"), None) => ScaleMode::Integer(3),
        (Some("native"), None) => ScaleMode::Native,
        (Some("fit"), None) => ScaleMode::Fit,
        (Some("fit-free"), None) => ScaleMode::FitFree,
        (Some("stretch"), None) => ScaleMode::Stretch,
        (Some(m @ ("native" | "fit" | "fit-free" | "stretch")), Some(_)) => {
            let shown = match (mode_flag, m) {
                (Some("--present"), "native") => "1to1",
                (Some("--present"), "fit") => "autofit",
                (Some("--present"), "fit-free") => "fit",
                _ => m,
            };
            return Err(format!(
                "--scale only applies to the integer presentation, not {} {shown}",
                mode_flag.unwrap_or("--mode")
            ));
        }
        (Some(m), _) => {
            return Err(format!(
                "--mode expects native, integer, fit, fit-free or stretch, got '{m}'"
            ))
        }
    };
    if filter_given && o.filter != Filter::Nearest && !o.mode.is_free() {
        return Err(
            "--filter bilinear/bicubic only applies to the non-integer presentations (--present fit or stretch, \
             --mode fit-free or stretch); the others are nearest"
                .into(),
        );
    }
    if preset_given && o.native_given {
        return Err("--native-preset and --native are exclusive".into());
    }
    o.native_given |= preset_given;
    if o.maximised && o.window_size.is_some() {
        return Err("--maximised and --window-size are exclusive".into());
    }
    if o.fullscreen && o.window_size.is_some() {
        return Err("--fullscreen and --window-size are exclusive".into());
    }
    if o.fullscreen && o.maximised {
        return Err("--fullscreen and --maximised are exclusive".into());
    }
    if o.content == Content::Image && o.image.is_none() {
        return Err("--content image needs --image file.png".into());
    }
    if o.decode.is_some() && o.image.is_some() {
        return Err("--decode reads its own PNG; do not combine it with --image".into());
    }
    Ok(Parsed::Run(Box::new(o)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Result<Parsed, String> {
        let args: Vec<String> = s.split_whitespace().map(String::from).collect();
        parse(&args)
    }

    fn opts(s: &str) -> Options {
        match p(s).unwrap() {
            Parsed::Run(o) => *o,
            other => panic!("not a run: {other:?}"),
        }
    }

    fn err(s: &str) -> String {
        p(s).unwrap_err()
    }

    #[test]
    fn defaults_match_the_previous_behaviour() {
        let o = opts("");
        assert_eq!(o.native, (256, 240));
        assert_eq!(o.mode, ScaleMode::Integer(3));
        assert_eq!(o.content, Content::Scene);
        assert_eq!(o.fps, 60);
        assert_eq!(o.bg, 0);
        assert_eq!(o.par, Par::SQUARE);
        assert_eq!(o.align, Align::Center);
    }

    #[test]
    fn existing_flags_keep_working() {
        // check-capture.ps1 usage.
        let o = opts("--image C:\\t\\known.png --scene image --scale 4");
        assert_eq!(o.content, Content::Image);
        assert_eq!(o.image.as_deref(), Some("C:\\t\\known.png"));
        assert_eq!(o.mode, ScaleMode::Integer(4));
        assert!(!o.native_given);
        let o = opts("--res 160x144 --scene card --scale 9");
        assert_eq!(o.native, (160, 144));
        assert!(o.native_given);
        assert_eq!(o.content, Content::Card);
        assert_eq!(o.mode, ScaleMode::Integer(9));
        assert_eq!(p("--version"), Ok(Parsed::Version));
        assert_eq!(p("-h"), Ok(Parsed::Help));
    }

    #[test]
    fn modes() {
        assert_eq!(opts("--mode native").mode, ScaleMode::Native);
        assert_eq!(opts("--mode integer").mode, ScaleMode::Integer(3));
        assert_eq!(opts("--mode integer --scale 10").mode, ScaleMode::Integer(10));
        assert_eq!(opts("--mode fit").mode, ScaleMode::Fit);
        let o = opts("--mode fit-free --filter bicubic");
        assert_eq!(o.mode, ScaleMode::FitFree);
        assert_eq!(o.filter, Filter::Bicubic);
        assert_eq!(opts("--mode fit-free --filter bilinear").filter, Filter::Bilinear);
        assert_eq!(opts("--mode fit --filter nearest").filter, Filter::Nearest);
    }

    #[test]
    fn layout_options() {
        let o = opts("--native 320x240 --par 8:7 --bg #102030 --align top-left");
        assert_eq!(o.native, (320, 240));
        assert_eq!(o.par, Par { n: 8, m: 7 });
        assert_eq!(o.bg, 0x102030);
        assert_eq!(o.align, Align::TopLeft);
        assert_eq!(opts("--bg FF00ff").bg, 0xFF00FF);
    }

    #[test]
    fn content_fps_and_flags() {
        for (name, c) in [
            ("card", Content::Card),
            ("bars", Content::Bars),
            ("ramp", Content::Ramp),
            ("checker", Content::Checker),
            ("motion", Content::Motion),
            ("text", Content::Text),
            ("noise", Content::Noise),
        ] {
            assert_eq!(opts(&format!("--content {name}")).content, c);
        }
        for f in [30, 50, 60, 75, 120] {
            assert_eq!(opts(&format!("--fps {f}")).fps, f);
        }
        let o = opts("--fixed-seed --borderless --maximised --no-round-corners --stats --exit-after 2.5 --counter");
        assert!(o.fixed_seed && o.borderless && o.maximised && o.no_round_corners && o.stats);
        assert_eq!(o.exit_after, Some(2.5));
        assert_eq!(o.counter, Some(true));
        assert_eq!(opts("--no-counter").counter, Some(false));
        assert_eq!(opts("--window-size 800x600").window_size, Some((800, 600)));
        assert_eq!(opts("--decode cap.png --mode fit").decode.as_deref(), Some("cap.png"));
    }

    #[test]
    fn present_vocabulary_maps_to_the_modes() {
        assert_eq!(opts("--present 1to1").mode, ScaleMode::Native);
        assert_eq!(opts("--present integer").mode, ScaleMode::Integer(3));
        assert_eq!(opts("--present integer --scale 7").mode, ScaleMode::Integer(7));
        assert_eq!(opts("--scale 7 --present integer").mode, ScaleMode::Integer(7));
        assert_eq!(opts("--present autofit").mode, ScaleMode::Fit);
        assert_eq!(opts("--present fit").mode, ScaleMode::FitFree);
        assert_eq!(opts("--present stretch").mode, ScaleMode::Stretch);
        assert_eq!(opts("--mode stretch").mode, ScaleMode::Stretch);
        let o = opts("--present fit --filter bilinear");
        assert_eq!((o.mode, o.filter), (ScaleMode::FitFree, Filter::Bilinear));
        let o = opts("--present stretch --filter bicubic");
        assert_eq!((o.mode, o.filter), (ScaleMode::Stretch, Filter::Bicubic));
        // Repeating the same flag keeps the last value.
        assert_eq!(opts("--present fit --present stretch").mode, ScaleMode::Stretch);
        for n in 1..=MAX_SCALE {
            assert_eq!(
                opts(&format!("--present integer --scale {n}")).mode,
                ScaleMode::Integer(n)
            );
        }
    }

    #[test]
    fn native_presets_par_and_bars() {
        for (name, size) in NATIVE_PRESETS {
            let o = opts(&format!("--native-preset {name}"));
            assert_eq!(o.native, size);
            assert!(o.native_given);
        }
        assert_eq!(opts("--native-preset snes").native, (256, 224));
        assert_eq!(opts("--native-preset hd1080").native, (1920, 1080));
        assert_eq!(opts("--par 1.2").par, Par { n: 1200, m: 1000 });
        assert_eq!(opts("--par 0.25").par, Par { n: 250, m: 1000 });
        assert_eq!(opts("--par 4").par, Par { n: 4000, m: 1000 });
        assert_eq!(opts("--bars black").bg, 0);
        assert_eq!(opts("--bars magenta").bg, 0xFF00FF);
        assert_eq!(opts("--bars Grey").bg, 0x202020);
        assert_eq!(opts("--bars #00ff00").bg, 0x00FF00);
        assert_eq!(opts("--bg red").bg, 0x800000);
        assert_eq!(opts("").bars_pattern, BarPattern::None);
        assert_eq!(opts("--bars-pattern checker").bars_pattern, BarPattern::Checker);
        assert_eq!(opts("--bars-pattern stripes").bars_pattern, BarPattern::Stripes);
        assert_eq!(opts("--bars-pattern noise").bars_pattern, BarPattern::Noise);
        let o = opts("--window 1000x700 --fixed-size --markers");
        assert_eq!(o.window_size, Some((1000, 700)));
        assert!(o.fixed_size);
        assert_eq!(o.markers, Some(true));
        assert_eq!(opts("--no-markers").markers, Some(false));
        assert_eq!(opts("").markers, None);
        assert!(!opts("").fixed_size);
    }

    #[test]
    fn content_aliases_and_grid() {
        assert_eq!(opts("--content testcard").content, Content::Card);
        assert_eq!(opts("--content gradient").content, Content::Ramp);
        assert_eq!(opts("--content grid").content, Content::Grid);
        assert_eq!(opts("--content scene").content, Content::Scene);
        for c in Content::ALL {
            assert_eq!(Content::parse(c.name()), Some(c));
        }
    }

    #[test]
    fn cycles_wrap_and_start_from_unknown_values() {
        assert_eq!(next_native_preset((256, 240)).1, (256, 224));
        assert_eq!(next_native_preset((1920, 1080)).1, (256, 240));
        assert_eq!(next_native_preset((123, 45)).1, (256, 240));
        let mut s = (256, 240);
        for _ in 0..NATIVE_PRESETS.len() {
            s = next_native_preset(s).1;
        }
        assert_eq!(s, (256, 240));
        assert_eq!(next_bar_color(0).1, 0x202020);
        assert_eq!(next_bar_color(0xFF00FF).1, 0);
        assert_eq!(next_bar_color(0x123456).1, 0);
        assert_eq!(bar_color_name(0xFF00FF), "magenta");
        assert_eq!(bar_color_name(0x123456), "#123456");
    }

    #[test]
    fn rejects_invalid_values() {
        assert!(err("--native 12x40").contains("between 16 and 8192"));
        assert!(err("--native abc").contains("WxH"));
        assert!(err("--native 256").contains("WxH"));
        assert!(err("--scale 0").contains("1 to 10"));
        assert!(err("--scale 11").contains("1 to 10"));
        assert!(err("--scale x").contains("1 to 10"));
        assert!(err("--mode zoom").contains("--mode expects"));
        assert!(err("--filter lanczos").contains("--filter expects"));
        assert!(err("--align middle").contains("--align expects"));
        assert!(err("--bg purple").contains("#rrggbb"));
        assert!(err("--bars #12345g").contains("--bars expects"));
        assert!(err("--bars-pattern dots").contains("--bars-pattern expects"));
        assert!(err("--native-preset psx").contains("--native-preset expects"));
        assert!(err("--present 2x").contains("--present expects"));
        assert!(err("--par 5").contains("decimal 0.25..4"));
        assert!(err("--par 0.1").contains("decimal 0.25..4"));
        assert!(err("--par inf").contains("decimal"));
        assert!(err("--window 8x8").contains("between 16 and 16384"));
        assert!(err("--window 800").contains("WxH"));
        assert!(err("--bg #12345").contains("#rrggbb"));
        assert!(err("--par 8").contains("N:M"));
        assert!(err("--par 0:7").contains("N:M"));
        assert!(err("--content video").contains("--content expects"));
        assert!(err("--fps 0").contains("1 to 240"));
        assert!(err("--fps 1000").contains("1 to 240"));
        assert!(err("--exit-after -1").contains("positive"));
        assert!(err("--exit-after nan").contains("positive"));
        assert!(err("--scale").contains("expects a value"));
        assert!(err("--bogus").contains("unknown argument --bogus"));
    }

    #[test]
    fn rejects_invalid_combinations() {
        assert!(
            err("--mode fit --scale 3").contains("--scale only applies to the integer presentation, not --mode fit")
        );
        assert!(err("--mode native --scale 2").contains("--scale only applies"));
        assert!(err("--mode fit-free --scale 2").contains("--scale only applies"));
        assert!(err("--present fit --scale 2").contains("not --present fit"));
        assert!(err("--present autofit --scale 2").contains("not --present autofit"));
        assert!(err("--present 1to1 --scale 2").contains("not --present 1to1"));
        assert!(err("--present stretch --scale 2").contains("not --present stretch"));
        assert!(err("--mode integer --filter bilinear").contains("only applies to the non-integer"));
        assert!(err("--filter bicubic").contains("only applies to the non-integer"));
        assert!(err("--present autofit --filter bilinear").contains("only applies to the non-integer"));
        assert!(err("--present fit --mode fit").contains("exclusive"));
        assert!(err("--mode fit --present fit").contains("exclusive"));
        assert!(err("--native 320x200 --native-preset nes").contains("exclusive"));
        assert!(err("--native-preset nes --res 320x200").contains("exclusive"));
        assert!(err("--maximised --window-size 800x600").contains("exclusive"));
        assert!(err("--fullscreen --window-size 800x600").contains("exclusive"));
        assert!(err("--fullscreen --maximised").contains("exclusive"));
        assert!(err("--content image").contains("needs --image"));
        assert!(err("--decode a.png --image b.png").contains("do not combine"));
    }
}
