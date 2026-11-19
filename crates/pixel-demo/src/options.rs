//! Command line parsing, pure and unit tested. Every invalid combination is rejected with a
//! message that names the flags involved.

use crate::{
    layout::{Align, Par, ScaleMode, MAX_SCALE},
    resample::Filter,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Content {
    Scene,
    Card,
    Bars,
    Ramp,
    Checker,
    Motion,
    Text,
    Noise,
    Image,
}

impl Content {
    pub const ALL: [Content; 9] = [
        Content::Scene,
        Content::Card,
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
            Content::Bars => "bars",
            Content::Ramp => "ramp",
            Content::Checker => "checker",
            Content::Motion => "motion",
            Content::Text => "text",
            Content::Noise => "noise",
            Content::Image => "image",
        }
    }

    pub fn parse(s: &str) -> Option<Content> {
        Content::ALL.into_iter().find(|c| c.name() == s)
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
    pub par: Par,
    pub content: Content,
    pub image: Option<String>,
    pub fps: u32,
    pub fixed_seed: bool,
    /// `Some(true)` forces the frame counter on, `Some(false)` off, `None` is on for `motion` only.
    pub counter: Option<bool>,
    pub borderless: bool,
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
            par: Par::SQUARE,
            content: Content::Scene,
            image: None,
            fps: 60,
            fixed_seed: false,
            counter: None,
            borderless: false,
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

pub const HELP: &str = "pixel-demo: universal test source for ultimatecrt
usage: pixel-demo [options]
  --native WxH        native image size (default 256x240; alias --res); any size 16..8192
  --mode M            native | integer | fit | fit-free (default integer)
  --scale N           integer scale 1..10 (mode integer only, default 3)
  --filter F          nearest | bilinear | bicubic (mode fit-free only, default nearest)
  --align A           center | top-left (default center)
  --bg #rrggbb        bar colour around the picture (default #000000)
  --par N:M           pixel aspect ratio, width:height of a pixel (e.g. 8:7; ignored by mode native)
  --content C         card | bars | ramp | checker | motion | text | noise | scene | image
                      (alias --scene; default scene)
  --image file.png    PNG source (content image; sets the native size unless --native is given)
  --fps N             update rate 1..240 (default 60), driven by a high-resolution timer
  --fixed-seed        deterministic noise (same pixels for the same frame number)
  --counter / --no-counter   binary frame counter in the top-left corner (default: motion only)
  --borderless        popup window without frame
  --maximised         start maximised
  --fullscreen        start fullscreen
  --no-round-corners  ask Windows 11 not to round the window corners
  --window-size WxH   initial client size (default: the picture size at the scale)
  --exit-after SECS   quit by itself
  --stats             print the achieved update rate every second
  --decode file.png   offline: print the frame counter of a client-area capture, using the
                      layout options above (mode, scale, native, par, align); exit code 1 if absent
  --version, -h/--help
keys: Space content, 1-9 and 0 integer scale 1-10, M mode, B filter, A align, F fullscreen,
      C counter, Esc quit";

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

pub fn parse_color(v: &str) -> Result<u32, String> {
    let h = v.strip_prefix('#').unwrap_or(v);
    if h.len() == 6 && h.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(u32::from_str_radix(h, 16).unwrap())
    } else {
        Err(format!("--bg expects #rrggbb, got '{v}'"))
    }
}

fn parse_par(v: &str) -> Result<Par, String> {
    let (a, b) = v
        .split_once(':')
        .ok_or_else(|| format!("--par expects N:M, got '{v}'"))?;
    match (a.parse::<u32>(), b.parse::<u32>()) {
        (Ok(n), Ok(m)) if (1..=1000).contains(&n) && (1..=1000).contains(&m) => Ok(Par { n, m }),
        _ => Err(format!("--par expects N:M with both 1..1000, got '{v}'")),
    }
}

/// Parses the arguments after the program name.
pub fn parse(args: &[String]) -> Result<Parsed, String> {
    let mut o = Options::default();
    let mut mode_name: Option<String> = None;
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
            "--mode" => mode_name = Some(value()?),
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
            "--bg" => o.bg = parse_color(&value()?)?,
            "--par" => o.par = parse_par(&value()?)?,
            "--content" | "--scene" => {
                let v = value()?;
                o.content = Content::parse(&v).ok_or_else(|| {
                    format!("{flag} expects card, bars, ramp, checker, motion, text, noise, scene or image, got '{v}'")
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
            "--borderless" => o.borderless = true,
            "--maximised" | "--maximized" => o.maximised = true,
            "--fullscreen" => o.fullscreen = true,
            "--no-round-corners" => o.no_round_corners = true,
            "--window-size" => {
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
        (Some(m @ ("native" | "fit" | "fit-free")), Some(_)) => {
            return Err(format!("--scale only applies to --mode integer, not --mode {m}"))
        }
        (Some(m), _) => return Err(format!("--mode expects native, integer, fit or fit-free, got '{m}'")),
    };
    if filter_given && o.filter != Filter::Nearest && o.mode != ScaleMode::FitFree {
        return Err("--filter bilinear/bicubic only applies to --mode fit-free (the other modes are nearest)".into());
    }
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
        assert!(err("--bg red").contains("#rrggbb"));
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
        assert!(err("--mode fit --scale 3").contains("--scale only applies to --mode integer"));
        assert!(err("--mode native --scale 2").contains("--scale only applies"));
        assert!(err("--mode fit-free --scale 2").contains("--scale only applies"));
        assert!(err("--mode integer --filter bilinear").contains("only applies to --mode fit-free"));
        assert!(err("--filter bicubic").contains("only applies to --mode fit-free"));
        assert!(err("--maximised --window-size 800x600").contains("exclusive"));
        assert!(err("--fullscreen --window-size 800x600").contains("exclusive"));
        assert!(err("--fullscreen --maximised").contains("exclusive"));
        assert!(err("--content image").contains("needs --image"));
        assert!(err("--decode a.png --image b.png").contains("do not combine"));
    }
}
