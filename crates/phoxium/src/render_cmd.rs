//! The `render` command: an offline render of a pattern or an image through the GPU pipeline, with
//! no window, written as an 8-bit PNG or a float PFM (UX-5, T-23).
//!
//! `phoxium render (--pattern name[:args] [--pattern-size WxH] | --image f.png) [--set id=value]...
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
    params::{self, Applied, ParamType, ParamValue, Params},
    patterns,
    presets::{self, Library, Origin, Preset},
    scaling, settings,
    source::Source,
    Res,
};

pub const USAGE: &str = "Usage: phoxium render (--pattern <name[:args]> [--pattern-size <WxH>] | --image <file.png>)
                         [--preset <id|file.toml>] [--source-colour <target[:variant]>] [--set <id=value>]...
                         [--size <WxH>] --out <file.png|file.pfm> [--validation]
  Renders one frame offline through the GPU pipeline (no window) and writes an 8-bit PNG, or a float
  PFM when the file name ends in .pfm. --size is the output size (default: the source size).
  --preset applies a tube preset (shipped or user id, or a preset file) before --source-colour and --set.
  --source-colour sets the colour target and its variant (number, id or name; the first without one), as
  `phoxium --help` describes; --set comes after it and wins.
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
    /// `--source-colour <target[:variant]>` (checked at parse), applied after the preset and before `--set`.
    pub colour: Option<crate::cli::ColourChoice>,
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
            "--source-colour" => {
                a.colour = Some(crate::cli::parse_colour("--source-colour", &value("--source-colour")?)?);
            }
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

/// Parses `text` as a value of the parameter `id` and stores it in `p` through `Params::set_checked`. A value
/// outside the hard range is clamped into it: the result is then the warning to show. A value that cannot be
/// read, a non-finite number (`nan`, `inf`) or an unknown enum name is an error.
pub fn set_param(p: &mut Params, id: &str, text: &str) -> Res<Option<String>> {
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
    match p.set_checked(id, value) {
        Ok(Applied::Exact) => Ok(None),
        Ok(Applied::Clamped(stored)) => Ok(Some(format!("--set {}", params::clamp_message(id, value, stored)))),
        Err(why) => Err(format!("--set {id}: value \"{text}\" is rejected ({why})").into()),
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
    let view =
        scaling::View::full(frame.width, frame.height).turned(crate::placement::Turn::from_params(params).is_turned());
    Ok(renderer
        .render(Some(sf), view, &Settings::default(), params, target, format)?
        .output)
}

/// A preset by file path (when `spec` names a file) or by id among the shipped and user presets.
///
/// The problems met on the way (a broken or duplicated user preset file, a value outside its soft range) are
/// written to stderr.
pub fn resolve_preset(spec: &str) -> Res<Preset> {
    let dir = settings::default_dir().map(|d| presets::user_dir(&d));
    let (preset, warnings) = resolve_preset_in(spec, dir.as_deref())?;
    for w in &warnings {
        eprintln!("warning: {w}");
    }
    Ok(preset)
}

/// [`resolve_preset`] with the user preset directory given; returns the preset and the warnings instead of
/// printing them.
pub fn resolve_preset_in(spec: &str, user_dir: Option<&Path>) -> Res<(Preset, Vec<String>)> {
    let path = Path::new(spec);
    if path.is_file() {
        let text = std::fs::read_to_string(path)?;
        let preset = Preset::parse(spec, &text, Origin::User(path.to_path_buf()))?;
        let warnings = preset.warnings.clone();
        return Ok((preset, warnings));
    }
    let lib = Library::load(user_dir);
    let found = lib.find(spec).cloned();
    match found {
        Some(p) => Ok((p, lib.warnings)),
        None => {
            let ids: Vec<&str> = lib.list().iter().map(|p| p.id.as_str()).collect();
            let mut msg = format!("--preset: no preset or file \"{spec}\" (presets: {})", ids.join(", "));
            for w in &lib.warnings {
                msg.push_str(&format!("\n  warning: {w}"));
            }
            Err(msg.into())
        }
    }
}

/// [`startup_params`] for the live app: the settings of `%APPDATA%\Phoxium`, the warnings on stderr.
pub fn live_params(preset_arg: Option<&str>) -> Res<Params> {
    let dir = settings::default_dir();
    let (params, warnings) = startup_params(preset_arg, dir.as_deref())?;
    for w in &warnings {
        eprintln!("warning: {w}");
    }
    Ok(params)
}

/// The parameters the live app starts with: the preset of `--preset` (`preset_arg`, an id or a file), else the
/// one of `settings.global.preset` (missing or empty: the table defaults; an unknown name is a warning), then
/// the `settings.global.overrides` of `dir\settings.toml` on top (T-51: the overrides are the ids that differ
/// from the preset). Returns the parameters and the warnings to show; an unknown `--preset` is an error. A bad
/// value in the preset or the overrides never fails: it is clamped (or skipped) with a warning naming the file
/// and the key ([`presets::apply_overrides_checked`]), also recorded as a menu notice.
pub fn startup_params(preset_arg: Option<&str>, dir: Option<&Path>) -> Res<(Params, Vec<String>)> {
    let mut warnings = Vec::new();
    let loaded = dir.map(settings::load);
    if let Some(w) = loaded.as_ref().and_then(|l| l.warning.clone()) {
        warnings.push(w);
    }
    let stored = loaded.map(|l| l.settings.global).unwrap_or_default();
    let user = dir.map(presets::user_dir);
    let mut params = Params::default();
    if let Some(spec) = preset_arg {
        let (preset, w) = resolve_preset_in(spec, user.as_deref())?;
        warnings.extend(w);
        params = preset.params();
    } else if !stored.preset.is_empty() {
        match resolve_preset_in(&stored.preset, user.as_deref()) {
            Ok((preset, w)) => {
                warnings.extend(w);
                params = preset.params();
            }
            Err(e) => warnings.push(format!("settings preset \"{}\" not used: {e}", stored.preset)),
        }
    }
    warnings.extend(presets::apply_overrides_checked(
        &mut params,
        &stored.overrides,
        settings::SETTINGS_FILE,
    ));
    Ok((params, warnings))
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
    if let Some(c) = a.colour {
        c.apply(&mut p, "--source-colour")?;
    }
    for (id, v) in &a.sets {
        if let Some(w) = set_param(&mut p, id, v)? {
            eprintln!("warning: {w}");
        }
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

/// Reads a colour PFM written by [`encode_pfm`] back to an RGBA image (alpha 1), rows top to bottom. Any
/// malformed input (truncated header, missing separator after the scale, non-finite or non-negative scale,
/// wrong data size) is an error, never a panic (R-33).
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
    if !scale.is_finite() || scale == 0.0 {
        return Err("invalid PFM scale".into());
    }
    if scale > 0.0 {
        return Err("only little-endian PFM is supported".into());
    }
    // Exactly one whitespace byte separates the header from the data.
    let data = bytes.get(pos + 1..).ok_or("truncated PFM: no data after the header")?;
    let expected = (w as usize)
        .checked_mul(h as usize)
        .and_then(|n| n.checked_mul(12))
        .ok_or("PFM size overflows")?;
    if data.len() != expected {
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
        // `--source-colour` is read and checked as on the live command line.
        assert_eq!(parse(&s(&["--out", "a.png"])).unwrap().colour, None);
        let c = parse(&s(&["--source-colour", "super_game_boy:2-a"]))
            .unwrap()
            .colour
            .unwrap();
        assert_eq!(
            (c.target, c.variant),
            (crate::params::SOURCE_COLOUR_TARGET_SUPER_GAME_BOY, 9)
        );
        assert!(parse(&s(&["--source-colour", "nope"])).is_err());
        assert!(parse(&s(&["--source-colour"])).is_err());
        assert!(USAGE.contains("--source-colour <target[:variant]>"));
    }

    fn temp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("phoxium-startup-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// `--preset` (an id), the settings preset when it is absent, and the settings overrides on top.
    #[test]
    fn startup_params_apply_the_preset_then_the_overrides() {
        let dir = temp("a");
        // No settings file, no preset: the table defaults.
        let (p, w) = startup_params(None, Some(&dir)).unwrap();
        assert_eq!(p, Params::default());
        assert!(w.is_empty(), "{w:?}");
        // --preset by id.
        let (p, w) = startup_params(Some("gdm-f520"), Some(&dir)).unwrap();
        assert_eq!(p, resolve_preset_in("gdm-f520", None).unwrap().0.params());
        assert!(w.is_empty(), "{w:?}");
        // The settings preset and overrides.
        let mut st = settings::Settings::default();
        st.global.preset = "bvm-20f1u".into();
        st.global.overrides.insert("exposure".into(), toml::Value::Float(0.5));
        st.global.overrides.insert("no_such".into(), toml::Value::Integer(1));
        settings::save(&dir, &st).unwrap();
        let (p, w) = startup_params(None, Some(&dir)).unwrap();
        let mut want = resolve_preset_in("bvm-20f1u", None).unwrap().0.params();
        want.exposure = 0.5;
        assert_eq!(p, want);
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("no_such"));
        // --preset wins over the settings preset; the overrides still apply on top.
        let (p, _) = startup_params(Some("gdm-f520"), Some(&dir)).unwrap();
        let mut want = resolve_preset_in("gdm-f520", None).unwrap().0.params();
        want.exposure = 0.5;
        assert_eq!(p, want);
        // An unknown --preset is an error naming the presets; an unknown settings preset is a warning.
        let e = startup_params(Some("nope"), Some(&dir)).unwrap_err().to_string();
        assert!(e.contains("nope") && e.contains("pvm-20m4u"), "{e}");
        st.global.preset = "gone".into();
        settings::save(&dir, &st).unwrap();
        let (p, w) = startup_params(None, Some(&dir)).unwrap();
        assert_eq!(p.exposure, 0.5);
        assert!(w.iter().any(|m| m.contains("gone")), "{w:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A preset file by path, a user preset by id, and the warnings that used to be dropped.
    #[test]
    fn resolve_preset_surfaces_library_and_parse_warnings() {
        let dir = temp("b");
        let user = presets::user_dir(&dir);
        std::fs::create_dir_all(&user).unwrap();
        let text = "schema = 1\nid = \"mine\"\nname = \"M\"\nlabel = \"l\"\n[params]\nexposure = 50.0\n[provenance]\nexposure = \"E: t\"\n";
        std::fs::write(user.join("a.toml"), text).unwrap();
        std::fs::write(user.join("b.toml"), text).unwrap();
        std::fs::write(user.join("bad.toml"), "schema = 2\n").unwrap();
        // By id: the duplicate id, the soft range and the broken file are all reported.
        let (p, w) = resolve_preset_in("mine", Some(&user)).unwrap();
        assert_eq!(p.params().exposure, 50.0);
        let all = w.join("\n");
        assert!(
            all.contains("already defined") && all.contains("soft range") && all.contains("bad.toml"),
            "{all}"
        );
        // By file path: the soft range warning of that file.
        let path = user.join("a.toml");
        let (_, w) = resolve_preset_in(&path.display().to_string(), None).unwrap();
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("soft range"));
        // An unknown id carries the library warnings in the error.
        let e = resolve_preset_in("nope", Some(&user)).unwrap_err().to_string();
        assert!(e.contains("bad.toml"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
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

    /// R-33: truncated or malformed headers are errors, never panics.
    #[test]
    fn pfm_truncated_or_malformed_input_is_an_error() {
        let img = ImageF32 {
            width: 2,
            height: 1,
            rgba: vec![0.5; 8],
        };
        let good = encode_pfm(&img);
        // Every prefix of a valid file (header cut anywhere, data cut short) is an error.
        for n in 0..good.len() {
            assert!(decode_pfm(&good[..n]).is_err(), "prefix of {n} bytes");
        }
        for bad in [
            &b"PF\n1 1\n-1.0"[..], // the header ends at the end of the file (A-13, B-12)
            b"PF\n1 1\nnan\n",
            b"PF\n1 1\ninf\n",
            b"PF\n1 1\n-inf\n",
            b"PF\n1 1\n0\n",
            b"PF\n1 1\n1.0\n",
            b"PF\n4294967295 4294967295\n-1.0\n",
            b"Pf\n1 1\n-1.0\n",
            b"PF\n-1 1\n-1.0\n",
            b"",
        ] {
            assert!(decode_pfm(bad).is_err(), "{:?}", String::from_utf8_lossy(bad));
        }
        // A trailing byte too many is an error as well.
        let mut long = good.clone();
        long.push(0);
        assert!(decode_pfm(&long).is_err());
    }

    /// `--set` goes through the hard range: clamped with a warning, NaN and infinity rejected.
    #[test]
    fn set_param_clamps_to_the_hard_range_and_rejects_non_finite_values() {
        let mut p = Params::default();
        let w = set_param(&mut p, "tube_aspect", "0").unwrap().expect("a clamp warning");
        assert!(w.contains("tube_aspect") && w.contains("valid range"), "{w}");
        assert_eq!(p.tube_aspect, 0.25);
        for text in ["nan", "inf", "-inf"] {
            let before = p;
            assert!(set_param(&mut p, "exposure", text).is_err(), "{text}");
            assert_eq!(p, before);
        }
        assert!(set_param(&mut p, "tube_white_xy", "0.3, NaN").is_err());
        assert_eq!(
            set_param(&mut p, "exposure", "50").unwrap(),
            None,
            "beyond the slider, inside the hard range"
        );
        assert_eq!(p.exposure, 50.0);
        assert!(set_param(&mut p, "amp_samples", "100000").unwrap().is_some());
        assert_eq!(p.amp_samples, 64);
    }

    /// A bad settings override never stops the startup: clamped, with a warning naming the file and the key.
    #[test]
    fn startup_params_clamp_bad_overrides_with_a_warning() {
        let dir = temp("c");
        let mut st = settings::Settings::default();
        st.global
            .overrides
            .insert("tube_visible_height_mm".into(), toml::Value::Float(0.0));
        st.global
            .overrides
            .insert("exposure".into(), toml::Value::Float(f64::NAN));
        st.global
            .overrides
            .insert("amp_samples".into(), toml::Value::Integer(100_000));
        settings::save(&dir, &st).unwrap();
        let (p, w) = startup_params(None, Some(&dir)).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(p.tube_visible_height_mm, 10.0);
        assert_eq!(p.exposure, Params::default().exposure, "NaN is ignored");
        assert_eq!(p.amp_samples, 64);
        assert_eq!(w.len(), 3, "{w:?}");
        for (key, word) in [
            ("tube_visible_height_mm", "valid range"),
            ("exposure", "not a finite number"),
            ("amp_samples", "valid range"),
        ] {
            assert!(
                w.iter()
                    .any(|m| m.starts_with("settings.toml: ") && m.contains(key) && m.contains(word)),
                "{key}: {w:?}"
            );
        }
        assert!(params::notices().iter().any(|n| n.contains("tube_visible_height_mm")));
    }
}
