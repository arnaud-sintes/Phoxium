//! The `render` command: an offline render of a pattern or an image through the GPU pipeline, with
//! no window, written as an 8-bit PNG or a float PFM (UX-5, T-23).
//!
//! `ultimatecrt render (--pattern name[:args] [--pattern-size WxH] | --image f.png) [--set id=value]...
//! [--size WxH] --out out.png|out.pfm [--gpu N] [--validation]`
//!
//! The settings are those of the live path: [`Settings::default`] and [`Params::default`] with the
//! `--set` overrides. The source frame is uploaded at its own size (view: full frame, divisor 1) and
//! encoded into a `--size` target (default: the source size), like the live path does for a window.
//! - `.png`: 8-bit RGB, rendered into the `B8G8R8A8_UNORM` swapchain format, the codes that the live
//!   output window would show.
//! - `.pfm`: 32-bit float RGB (little-endian, scale -1, rows bottom to top as the format requires),
//!   rendered into `R32G32B32A32_SFLOAT`: the same encoded values, not quantised to 8 bits.
//!   The PFM values are display codes in [0, 1] after the `encode` stage, not linear light.

use std::path::Path;

use crate::{
    gpu::{Headless, ImageF32, Settings, SourceFrame, TargetFormat},
    params::{self, ParamType, ParamValue, Params},
    patterns,
    presets::{self, Library, Origin, Preset},
    scaling, settings,
    source::Source,
    Res,
};

pub const USAGE: &str =
    "Usage: ultimatecrt render (--pattern <name[:args]> [--pattern-size <WxH>] | --image <file.png>)
                         [--preset <id|file.toml>] [--set <id=value>]... [--size <WxH>] --out <file.png|file.pfm>
                         [--validation]
  Renders one frame offline through the GPU pipeline (no window) and writes an 8-bit PNG, or a float
  PFM when the file name ends in .pfm. --size is the output size (default: the source size).
  --preset applies a tube preset (shipped or user id, or a preset file) before --set.
  --set overrides a parameter by id; vectors are comma separated, booleans are true/false, enums
  take the value name.";

/// Options of the `render` command.
#[derive(Debug, Default)]
pub struct RenderArgs {
    pub pattern: Option<String>,
    pub pattern_size: Option<(u32, u32)>,
    pub image: Option<String>,
    /// Tube preset id or file, applied before `--set`.
    pub preset: Option<String>,
    pub sets: Vec<(String, String)>,
    pub size: Option<(u32, u32)>,
    pub out: Option<String>,
    pub validation: bool,
}

/// Parses the arguments after `render`.
pub fn parse(args: &[String]) -> Res<RenderArgs> {
    let mut a = RenderArgs::default();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = |name: &str| it.next().cloned().ok_or_else(|| format!("{name} expects a value"));
        match arg.as_str() {
            "--pattern" => a.pattern = Some(value("--pattern")?),
            "--pattern-size" => a.pattern_size = Some(patterns::parse_size(&value("--pattern-size")?)?),
            "--image" => a.image = Some(value("--image")?),
            "--size" => a.size = Some(patterns::parse_size(&value("--size")?)?),
            "--out" => a.out = Some(value("--out")?),
            "--preset" => a.preset = Some(value("--preset")?),
            "--validation" => a.validation = true,
            "--set" => {
                let v = value("--set")?;
                let (id, val) = v.split_once('=').ok_or("--set expects id=value")?;
                a.sets.push((id.to_string(), val.to_string()));
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("render: unknown argument {other}").into()),
        }
    }
    Ok(a)
}

/// Parses `text` as a value of the parameter `id` and stores it in `p`.
pub fn set_param(p: &mut Params, id: &str, text: &str) -> Res<()> {
    let meta = params::meta(id).ok_or_else(|| format!("--set: unknown parameter \"{id}\""))?;
    let bad = || format!("--set {id}: cannot read \"{text}\" as {:?}", meta.ty);
    let floats = |n: usize| -> Res<Vec<f32>> {
        let v: Vec<f32> = text
            .split(',')
            .map(|s| s.trim().parse::<f32>().map_err(|_| bad()))
            .collect::<Result<_, _>>()?;
        if v.len() == n {
            Ok(v)
        } else {
            Err(format!("--set {id}: expects {n} comma separated numbers").into())
        }
    };
    let value = match meta.ty {
        ParamType::F32 => ParamValue::F32(text.trim().parse().map_err(|_| bad())?),
        ParamType::I32 => ParamValue::I32(text.trim().parse().map_err(|_| bad())?),
        ParamType::Bool => ParamValue::Bool(match text.trim() {
            "true" | "1" | "on" => true,
            "false" | "0" | "off" => false,
            _ => return Err(bad().into()),
        }),
        ParamType::Enum => ParamValue::Enum(
            meta.values
                .iter()
                .position(|v| *v == text.trim())
                .ok_or_else(|| format!("--set {id}: expected one of {:?}", meta.values))? as u32,
        ),
        ParamType::Vec2 => ParamValue::Vec2(floats(2)?.try_into().unwrap()),
        ParamType::Vec3 => ParamValue::Vec3(floats(3)?.try_into().unwrap()),
        ParamType::Vec4 => ParamValue::Vec4(floats(4)?.try_into().unwrap()),
    };
    if p.set(id, value) {
        Ok(())
    } else {
        Err(format!("--set {id}: value \"{text}\" is rejected").into())
    }
}

/// Renders the first frame of `source` with `renderer`, as the live path would, into a `size` target
/// (default: the source size) of `format`.
pub fn render_source(
    renderer: &mut Headless,
    source: &Source,
    params: &Params,
    size: Option<(u32, u32)>,
    format: TargetFormat,
) -> Res<ImageF32> {
    let frame = source
        .frame(0)?
        .ok_or("render needs --pattern or --image (a capture cannot be rendered offline)")?;
    let target = size.unwrap_or((frame.width, frame.height));
    let sf = SourceFrame {
        width: frame.width,
        height: frame.height,
        bgra: &frame.data,
    };
    let view = scaling::View::full(frame.width, frame.height);
    Ok(renderer
        .render(Some(sf), view, &Settings::default(), params, target, format)?
        .output)
}

/// A preset by file path (when `spec` names a file) or by id among the shipped and user presets.
pub fn resolve_preset(spec: &str) -> Res<Preset> {
    let path = Path::new(spec);
    if path.is_file() {
        let text = std::fs::read_to_string(path)?;
        return Ok(Preset::parse(spec, &text, Origin::User(path.to_path_buf()))?);
    }
    let dir = settings::default_dir().map(|d| presets::user_dir(&d));
    let lib = Library::load(dir.as_deref());
    lib.find(spec).cloned().ok_or_else(|| {
        let ids: Vec<&str> = lib.list().iter().map(|p| p.id.as_str()).collect();
        format!("--preset: no preset or file \"{spec}\" (presets: {})", ids.join(", ")).into()
    })
}

/// Runs the `render` command with the arguments after `render`.
pub fn run(args: &[String]) -> Res<()> {
    let a = parse(args)?;
    let out = a.out.as_deref().ok_or("render: --out is required")?;
    let float = match Path::new(out)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_lowercase)
    {
        Some(e) if e == "png" => false,
        Some(e) if e == "pfm" => true,
        _ => return Err("render: --out must end in .png or .pfm".into()),
    };
    let source = Source::choose(a.pattern.as_deref(), a.pattern_size, a.image.as_deref())?;
    let mut p = Params::default();
    if let Some(preset) = &a.preset {
        p = resolve_preset(preset)?.params();
    }
    for (id, v) in &a.sets {
        set_param(&mut p, id, v)?;
    }
    let mut renderer = Headless::new(a.validation)?;
    let format = if float {
        TargetFormat::Rgba32F
    } else {
        TargetFormat::Bgra8Unorm
    };
    let img = render_source(&mut renderer, &source, &p, a.size, format)?;
    if float {
        std::fs::write(out, encode_pfm(&img))?;
    } else {
        std::fs::write(out, encode_png(&img)?)?;
    }
    println!("rendered {}x{} to {out}", img.width, img.height);
    Ok(())
}

/// 8-bit RGB PNG of an image whose values are `code / 255` (alpha dropped).
pub fn encode_png(img: &ImageF32) -> Res<Vec<u8>> {
    let mut rgb = Vec::with_capacity(img.width as usize * img.height as usize * 3);
    for p in img.rgba.as_chunks::<4>().0 {
        rgb.extend(p[..3].iter().map(|v| (v * 255.0).round().clamp(0.0, 255.0) as u8));
    }
    let mut bytes = Vec::new();
    let mut enc = png::Encoder::new(&mut bytes, img.width, img.height);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(&rgb)?;
    Ok(bytes)
}

/// Little-endian colour PFM of an image (alpha dropped), rows bottom to top.
pub fn encode_pfm(img: &ImageF32) -> Vec<u8> {
    let mut out = format!("PF\n{} {}\n-1.0\n", img.width, img.height).into_bytes();
    for y in (0..img.height).rev() {
        for x in 0..img.width {
            for v in &img.pixel(x, y)[..3] {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
    }
    out
}

/// Reads a colour PFM written by [`encode_pfm`] back to an RGBA image (alpha 1), rows top to bottom.
pub fn decode_pfm(bytes: &[u8]) -> Res<ImageF32> {
    let mut pos = 0;
    let mut token = || -> Res<String> {
        while bytes.get(pos).is_some_and(u8::is_ascii_whitespace) {
            pos += 1;
        }
        let start = pos;
        while bytes.get(pos).is_some_and(|b| !b.is_ascii_whitespace()) {
            pos += 1;
        }
        if start == pos {
            return Err("truncated PFM header".into());
        }
        Ok(String::from_utf8_lossy(&bytes[start..pos]).into_owned())
    };
    if token()? != "PF" {
        return Err("not a colour PFM".into());
    }
    let w: u32 = token()?.parse()?;
    let h: u32 = token()?.parse()?;
    let scale: f32 = token()?.parse()?;
    if scale >= 0.0 {
        return Err("only little-endian PFM is supported".into());
    }
    let data = &bytes[pos + 1..];
    if data.len() != w as usize * h as usize * 12 {
        return Err("PFM data size does not match its header".into());
    }
    let mut rgba = vec![1.0f32; w as usize * h as usize * 4];
    for (i, px) in data.as_chunks::<12>().0.iter().enumerate() {
        let (x, row) = (i % w as usize, i / w as usize);
        let y = h as usize - 1 - row;
        for c in 0..3 {
            let b: [u8; 4] = px[c * 4..c * 4 + 4].try_into().unwrap();
            rgba[(y * w as usize + x) * 4 + c] = f32::from_le_bytes(b);
        }
    }
    Ok(ImageF32 {
        width: w,
        height: h,
        rgba,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn parses_arguments() {
        let a = parse(&s(&[
            "--pattern",
            "ramp",
            "--size",
            "64x32",
            "--out",
            "a.png",
            "--set",
            "exposure=0.5",
        ]))
        .unwrap();
        assert_eq!(a.pattern.as_deref(), Some("ramp"));
        assert_eq!(a.size, Some((64, 32)));
        assert_eq!(a.sets, [("exposure".to_string(), "0.5".to_string())]);
        assert!(parse(&s(&["--bogus"])).is_err());
        assert!(parse(&s(&["--set", "novalue"])).is_err());
    }

    #[test]
    fn set_param_reads_typed_values() {
        let mut p = Params::default();
        set_param(&mut p, "exposure", "0.5").unwrap();
        assert_eq!(p.get("exposure"), Some(ParamValue::F32(0.5)));
        assert!(set_param(&mut p, "exposure", "abc").is_err());
        assert!(set_param(&mut p, "no_such", "1").is_err());
        set_param(&mut p, "tube_white_xy", "0.3, 0.33").unwrap();
        assert_eq!(p.get("tube_white_xy"), Some(ParamValue::Vec2([0.3, 0.33])));
        assert!(set_param(&mut p, "tube_white_xy", "0.3").is_err());
    }

    #[test]
    fn pfm_round_trips_exactly() {
        let img = ImageF32 {
            width: 3,
            height: 2,
            rgba: (0..24).map(|i| if i % 4 == 3 { 1.0 } else { i as f32 / 7.0 }).collect(),
        };
        assert_eq!(decode_pfm(&encode_pfm(&img)).unwrap(), img);
    }
}
