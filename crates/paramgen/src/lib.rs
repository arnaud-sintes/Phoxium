//! Declarative parameter table (TEC-3, DESIGN section 8 "defined once").
//!
//! Every tunable parameter is defined once, in `params/<stage>.toml` at the workspace root. This crate reads
//! those files, validates them and generates:
//! - Rust: a `Params` struct with its defaults, a metadata array (`PARAMS_META`), std140 serialisation and
//!   access by id (`crate::params` in `ultimatecrt` includes it from `OUT_DIR`);
//! - GLSL: `params.glsl`, a uniform block whose members carry explicit `layout(offset = N)`.
//!
//! The std140 layout is computed here, once, and written into both outputs. The GLSL compiler only rejects
//! overlapping or misaligned offsets, so the unit tests compare the offsets with hand-computed values.
//!
//! File format, one `[[param]]` table per parameter:
//!
//! ```toml
//! [[param]]
//! id = "exposure"            # snake_case, unique across all files
//! stage = "output"           # must equal the file stem
//! type = "f32"               # f32, i32, bool, enum, vec2, vec3, vec4
//! unit = "1"                 # "1" for dimensionless
//! default = 1.0              # an array for vecN, a string for enum
//! min = 0.05                 # soft range, required for numeric types, per component for vecN
//! max = 8.0
//! tag = "E"                  # confidence: S sourced, D derived, K standard knowledge, E estimate
//! source = "QUESTIONS COL-1"
//! description = "..."
//! group = "Output"           # UI group
//! # values = ["a", "b"]      # enum only: the choices, snake_case
//! ```
//!
//! Files whose name starts with `_` (such as `_test.toml`) hold test-only entries: they are loaded only when
//! asked (debug builds) and placed after the production files, so production offsets do not depend on them.

use serde::Deserialize;
use std::{collections::HashMap, fmt, fs, path::Path};

/// A table error. The message names the file and, when known, the parameter id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

fn err<T>(msg: String) -> Result<T, Error> {
    Err(Error(msg))
}

/// Type of a parameter, with its std140 layout and its Rust and GLSL spellings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Type {
    F32,
    I32,
    Bool,
    Enum,
    Vec2,
    Vec3,
    Vec4,
}

impl Type {
    fn parse(s: &str) -> Option<Type> {
        Some(match s {
            "f32" => Type::F32,
            "i32" => Type::I32,
            "bool" => Type::Bool,
            "enum" => Type::Enum,
            "vec2" => Type::Vec2,
            "vec3" => Type::Vec3,
            "vec4" => Type::Vec4,
            _ => return None,
        })
    }

    /// Number of 32-bit components.
    pub fn components(self) -> usize {
        match self {
            Type::Vec2 => 2,
            Type::Vec3 => 3,
            Type::Vec4 => 4,
            _ => 1,
        }
    }

    /// Size in bytes in a std140 block.
    pub fn std140_size(self) -> usize {
        4 * self.components()
    }

    /// Base alignment in bytes in a std140 block (GLSL 4.50 section 7.6.2.2, rules 1 to 3).
    pub fn std140_align(self) -> usize {
        match self {
            Type::Vec2 => 8,
            Type::Vec3 | Type::Vec4 => 16,
            _ => 4,
        }
    }

    fn rust(self) -> &'static str {
        match self {
            Type::F32 => "f32",
            Type::I32 => "i32",
            Type::Bool => "bool",
            Type::Enum => "u32",
            Type::Vec2 => "[f32; 2]",
            Type::Vec3 => "[f32; 3]",
            Type::Vec4 => "[f32; 4]",
        }
    }

    fn glsl(self) -> &'static str {
        match self {
            Type::F32 => "float",
            Type::I32 => "int",
            Type::Bool => "bool",
            Type::Enum => "uint",
            Type::Vec2 => "vec2",
            Type::Vec3 => "vec3",
            Type::Vec4 => "vec4",
        }
    }

    fn variant(self) -> &'static str {
        match self {
            Type::F32 => "F32",
            Type::I32 => "I32",
            Type::Bool => "Bool",
            Type::Enum => "Enum",
            Type::Vec2 => "Vec2",
            Type::Vec3 => "Vec3",
            Type::Vec4 => "Vec4",
        }
    }

    fn is_numeric(self) -> bool {
        !matches!(self, Type::Bool | Type::Enum)
    }
}

/// A validated default value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    F32(f32),
    I32(i32),
    Bool(bool),
    /// Index into `Param::values`.
    Enum(u32),
    Vec(Vec<f32>),
}

/// One validated parameter with its std140 offset.
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    /// Path of the defining file relative to the workspace, such as `params/output.toml`.
    pub file: String,
    pub id: String,
    pub stage: String,
    pub ty: Type,
    pub unit: String,
    pub default: Value,
    /// Soft range as written in the file (UI slider range; values outside are allowed with a warning), per
    /// component for vecN. `None` for bool and enum.
    pub soft_range: Option<(f64, f64)>,
    /// Confidence tag: `S`, `D`, `K` or `E`.
    pub tag: char,
    pub source: String,
    pub description: String,
    pub group: String,
    /// Enum choices (snake_case), empty for other types.
    pub values: Vec<String>,
    /// Byte offset in the std140 block.
    pub offset: usize,
}

/// The whole table, in layout order.
#[derive(Debug, Clone, PartialEq)]
pub struct Table {
    pub params: Vec<Param>,
    /// Size of the std140 block in bytes, rounded up to 16.
    pub size: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    #[serde(default)]
    param: Vec<RawParam>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawParam {
    id: String,
    stage: String,
    #[serde(rename = "type")]
    ty: String,
    unit: String,
    default: toml::Value,
    min: Option<toml::Value>,
    max: Option<toml::Value>,
    tag: String,
    source: String,
    description: String,
    group: String,
    #[serde(default)]
    values: Vec<String>,
}

fn is_snake_ident(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some('a'..='z'))
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && !s.contains("__")
        && !s.ends_with('_')
}

fn number(v: &toml::Value) -> Option<f64> {
    match v {
        toml::Value::Float(f) => Some(*f),
        toml::Value::Integer(i) => Some(*i as f64),
        _ => None,
    }
}

/// Parses one TOML file. `file` is the name used in messages (such as `params/output.toml`), `stem` the
/// expected `stage` of every entry. Offsets are left at 0 (see [`build`]).
pub fn parse_file(file: &str, stem: &str, text: &str) -> Result<Vec<Param>, Error> {
    let raw: RawFile = toml::from_str(text).map_err(|e| Error(format!("{file}: {e}")))?;
    raw.param.into_iter().map(|r| validate(file, stem, r)).collect()
}

fn validate(file: &str, stem: &str, r: RawParam) -> Result<Param, Error> {
    let id = r.id;
    let at = format!("{file}: parameter `{id}`");
    if !is_snake_ident(&id) {
        return err(format!(
            "{at}: the id must be snake_case (a-z, 0-9, single underscores)"
        ));
    }
    if r.stage != stem {
        return err(format!(
            "{at}: stage `{}` does not match the file name `{stem}`",
            r.stage
        ));
    }
    let Some(ty) = Type::parse(&r.ty) else {
        return err(format!(
            "{at}: unknown type `{}` (f32, i32, bool, enum, vec2, vec3, vec4)",
            r.ty
        ));
    };
    let tag = match r.tag.as_str() {
        "S" | "D" | "K" | "E" => r.tag.chars().next().unwrap_or('E'),
        _ => return err(format!("{at}: tag `{}` must be one of S, D, K, E", r.tag)),
    };
    for (name, text) in [
        ("unit", &r.unit),
        ("source", &r.source),
        ("description", &r.description),
        ("group", &r.group),
    ] {
        if text.trim().is_empty() {
            return err(format!("{at}: `{name}` is empty"));
        }
    }
    if ty != Type::Enum && !r.values.is_empty() {
        return err(format!("{at}: `values` is only allowed for type enum"));
    }

    // Soft range.
    let soft_range = if ty.is_numeric() {
        let (Some(min), Some(max)) = (&r.min, &r.max) else {
            return err(format!("{at}: numeric parameters need `min` and `max` (soft range)"));
        };
        let (Some(min), Some(max)) = (number(min), number(max)) else {
            return err(format!("{at}: `min` and `max` must be numbers"));
        };
        if ty == Type::I32 && (min.fract() != 0.0 || max.fract() != 0.0) {
            return err(format!("{at}: `min` and `max` of an i32 must be integers"));
        }
        if !(min.is_finite() && max.is_finite()) || min > max {
            return err(format!("{at}: invalid soft range [{min}, {max}]"));
        }
        Some((min, max))
    } else {
        if r.min.is_some() || r.max.is_some() {
            return err(format!("{at}: `min` and `max` are not allowed for type {}", r.ty));
        }
        None
    };

    // Default.
    // f32 values are compared with bounds rounded to f32, so a default equal to a bound such as 0.3 is in range.
    let round = |x: f64| if ty == Type::I32 { x } else { x as f32 as f64 };
    let in_range = |v: f64| soft_range.is_none_or(|(lo, hi)| (round(lo)..=round(hi)).contains(&v));
    let out_of_range = |shown: String| {
        let (lo, hi) = soft_range.unwrap_or_default();
        err(format!("{at}: default {shown} is outside the soft range [{lo}, {hi}]"))
    };
    let default = match ty {
        Type::F32 => {
            let Some(v) = number(&r.default) else {
                return err(format!("{at}: the default of an f32 must be a number"));
            };
            let v = v as f32;
            if !v.is_finite() {
                return err(format!("{at}: the default is not finite"));
            }
            if !in_range(v as f64) {
                return out_of_range(format!("{v}"));
            }
            Value::F32(v)
        }
        Type::I32 => {
            let toml::Value::Integer(v) = r.default else {
                return err(format!("{at}: the default of an i32 must be an integer"));
            };
            let Ok(v) = i32::try_from(v) else {
                return err(format!("{at}: the default {v} does not fit in an i32"));
            };
            if !in_range(v as f64) {
                return out_of_range(format!("{v}"));
            }
            Value::I32(v)
        }
        Type::Bool => {
            let toml::Value::Boolean(v) = r.default else {
                return err(format!("{at}: the default of a bool must be true or false"));
            };
            Value::Bool(v)
        }
        Type::Enum => {
            if r.values.is_empty() {
                return err(format!("{at}: an enum needs a non-empty `values` list"));
            }
            for (i, v) in r.values.iter().enumerate() {
                if !is_snake_ident(v) {
                    return err(format!("{at}: enum value `{v}` must be snake_case"));
                }
                if r.values[..i].contains(v) {
                    return err(format!("{at}: enum value `{v}` is listed twice"));
                }
            }
            let toml::Value::String(v) = &r.default else {
                return err(format!(
                    "{at}: the default of an enum must be one of its values, as a string"
                ));
            };
            let Some(index) = r.values.iter().position(|x| x == v) else {
                return err(format!("{at}: default `{v}` is not one of {:?}", r.values));
            };
            Value::Enum(index as u32)
        }
        Type::Vec2 | Type::Vec3 | Type::Vec4 => {
            let n = ty.components();
            let toml::Value::Array(items) = &r.default else {
                return err(format!("{at}: the default of a vec{n} must be an array of {n} numbers"));
            };
            let values: Option<Vec<f32>> = items.iter().map(|x| number(x).map(|v| v as f32)).collect();
            let Some(values) = values.filter(|v| v.len() == n) else {
                return err(format!("{at}: the default of a vec{n} must be an array of {n} numbers"));
            };
            if values.iter().any(|v| !v.is_finite()) {
                return err(format!("{at}: the default is not finite"));
            }
            if !values.iter().all(|&v| in_range(v as f64)) {
                return out_of_range(format!("{values:?}"));
            }
            Value::Vec(values)
        }
    };

    Ok(Param {
        file: file.to_string(),
        id,
        stage: r.stage,
        ty,
        unit: r.unit,
        default,
        soft_range,
        tag,
        source: r.source,
        description: r.description,
        group: r.group,
        values: r.values,
        offset: 0,
    })
}

fn align_up(x: usize, a: usize) -> usize {
    x.div_ceil(a) * a
}

/// Builds the table from `(file name, file stem, text)` triples, in the given order: validates every entry,
/// rejects duplicate ids (naming both files) and computes the std140 offsets in declaration order.
pub fn build(files: &[(String, String, String)]) -> Result<Table, Error> {
    let mut params: Vec<Param> = Vec::new();
    let mut seen: HashMap<String, String> = HashMap::new();
    let mut enum_consts: HashMap<String, String> = HashMap::new();
    for (file, stem, text) in files {
        for p in parse_file(file, stem, text)? {
            if let Some(first) = seen.get(&p.id) {
                return err(format!(
                    "{file}: duplicate parameter id `{}` (first defined in {first})",
                    p.id
                ));
            }
            seen.insert(p.id.clone(), file.clone());
            for v in &p.values {
                let name = enum_const_name(&p.id, v);
                if let Some(other) = enum_consts.insert(name.clone(), p.id.clone()) {
                    return err(format!(
                        "{file}: parameter `{}`: enum constant {name} collides with parameter `{other}`",
                        p.id
                    ));
                }
            }
            params.push(p);
        }
    }
    if params.is_empty() {
        return err("the parameter table is empty".to_string());
    }
    let mut cursor = 0;
    for p in &mut params {
        p.offset = align_up(cursor, p.ty.std140_align());
        cursor = p.offset + p.ty.std140_size();
    }
    Ok(Table {
        params,
        size: align_up(cursor, 16),
    })
}

/// Loads every `*.toml` of `dir`: production files sorted by name, then the `_`-prefixed test files (sorted)
/// when `include_test` is true. Messages name files as `params/<name>`.
pub fn load_dir(dir: &Path, include_test: bool) -> Result<Table, Error> {
    let entries = fs::read_dir(dir).map_err(|e| Error(format!("cannot read {}: {e}", dir.display())))?;
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| Error(format!("cannot read {}: {e}", dir.display())))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".toml") && (include_test || !name.starts_with('_')) {
            names.push(name);
        }
    }
    names.sort_by_key(|n| (n.starts_with('_'), n.clone()));
    let mut files = Vec::new();
    for name in names {
        let path = dir.join(&name);
        let text = fs::read_to_string(&path).map_err(|e| Error(format!("cannot read {}: {e}", path.display())))?;
        let stem = name.trim_end_matches(".toml").to_string();
        files.push((format!("params/{name}"), stem, text));
    }
    build(&files)
}

/// Name of the constant generated for an enum value, in Rust and GLSL: `<ID>_<VALUE>` in upper case.
pub fn enum_const_name(id: &str, value: &str) -> String {
    format!("{}_{}", id.to_uppercase(), value.to_uppercase())
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn rust_f32(v: f32) -> String {
    format!("{v:?}_f32")
}

fn rust_f64(v: f64) -> String {
    format!("{v:?}_f64")
}

fn rust_value(p: &Param) -> String {
    match &p.default {
        Value::F32(v) => rust_f32(*v),
        Value::I32(v) => format!("{v}_i32"),
        Value::Bool(v) => format!("{v}"),
        Value::Enum(v) => format!("{v}_u32"),
        Value::Vec(v) => format!("[{}]", v.iter().map(|x| rust_f32(*x)).collect::<Vec<_>>().join(", ")),
    }
}

fn confidence(tag: char) -> &'static str {
    match tag {
        'S' => "Sourced",
        'D' => "Derived",
        'K' => "Known",
        _ => "Estimate",
    }
}

/// Rust source to `include!` in a module that defines `ParamMeta`, `ParamType`, `ParamValue` and `Confidence`
/// (see `crates/ultimatecrt/src/params.rs`).
pub fn rust_source(t: &Table) -> String {
    let mut s = String::new();
    s.push_str("// Generated by crates/paramgen from params/*.toml. Do not edit: change the TOML files.\n\n");

    for p in &t.params {
        for (i, v) in p.values.iter().enumerate() {
            s.push_str(&format!(
                "/// Value `{v}` of the enum parameter `{}`.\npub const {}: u32 = {i};\n",
                p.id,
                enum_const_name(&p.id, v)
            ));
        }
    }

    s.push_str("\n/// Every parameter of the table, with the Rust type of its std140 member.\n");
    s.push_str("#[derive(Debug, Clone, Copy, PartialEq)]\npub struct Params {\n");
    for p in &t.params {
        s.push_str(&format!(
            "    /// {} Unit: {}. [{}] ({})\n    pub {}: {},\n",
            one_line(&p.description),
            p.unit,
            p.tag,
            p.file,
            p.id,
            p.ty.rust()
        ));
    }
    s.push_str("}\n\n");

    s.push_str("impl Default for Params {\n    fn default() -> Self {\n        Self {\n");
    for p in &t.params {
        s.push_str(&format!("            {}: {},\n", p.id, rust_value(p)));
    }
    s.push_str("        }\n    }\n}\n\n");

    s.push_str(&format!(
        "/// Size in bytes of the std140 uniform block (`params.glsl`).\npub const PARAMS_STD140_SIZE: usize = {};\n\n",
        t.size
    ));

    s.push_str("/// Metadata of every parameter, in layout order.\npub const PARAMS_META: &[ParamMeta] = &[\n");
    for p in &t.params {
        let default = format!("ParamValue::{}({})", p.ty.variant(), rust_value(p));
        let range = match p.soft_range {
            Some((lo, hi)) => format!("Some(({}, {}))", rust_f64(lo), rust_f64(hi)),
            None => "None".to_string(),
        };
        s.push_str(&format!(
            "    ParamMeta {{\n        id: {:?},\n        stage: {:?},\n        ty: ParamType::{},\n        unit: {:?},\n        default: {default},\n        soft_range: {range},\n        confidence: Confidence::{},\n        source: {:?},\n        description: {:?},\n        group: {:?},\n        values: &{:?},\n        file: {:?},\n        offset: {},\n        size: {},\n    }},\n",
            p.id,
            p.stage,
            p.ty.variant(),
            p.unit,
            confidence(p.tag),
            one_line(&p.source),
            one_line(&p.description),
            p.group,
            p.values,
            p.file,
            p.offset,
            p.ty.std140_size(),
        ));
    }
    s.push_str("];\n\n");

    s.push_str("impl Params {\n");
    s.push_str("    /// Writes the std140 uniform block (little endian, padding zeroed).\n");
    s.push_str("    pub fn write_std140(&self, out: &mut [u8; PARAMS_STD140_SIZE]) {\n        out.fill(0);\n");
    for p in &t.params {
        let o = p.offset;
        match p.ty {
            Type::Bool => s.push_str(&format!(
                "        out[{o}..{}].copy_from_slice(&u32::from(self.{}).to_le_bytes());\n",
                o + 4,
                p.id
            )),
            Type::Vec2 | Type::Vec3 | Type::Vec4 => {
                for c in 0..p.ty.components() {
                    let a = o + 4 * c;
                    s.push_str(&format!(
                        "        out[{a}..{}].copy_from_slice(&self.{}[{c}].to_le_bytes());\n",
                        a + 4,
                        p.id
                    ));
                }
            }
            _ => s.push_str(&format!(
                "        out[{o}..{}].copy_from_slice(&self.{}.to_le_bytes());\n",
                o + 4,
                p.id
            )),
        }
    }
    s.push_str("    }\n\n");

    s.push_str("    /// The std140 uniform block as a byte array.\n");
    s.push_str("    pub fn to_std140(&self) -> [u8; PARAMS_STD140_SIZE] {\n");
    s.push_str(
        "        let mut out = [0; PARAMS_STD140_SIZE];\n        self.write_std140(&mut out);\n        out\n    }\n\n",
    );

    s.push_str("    /// Current value of the parameter `id`, `None` for an unknown id.\n");
    s.push_str("    pub fn get(&self, id: &str) -> Option<ParamValue> {\n        match id {\n");
    for p in &t.params {
        s.push_str(&format!(
            "            {:?} => Some(ParamValue::{}(self.{})),\n",
            p.id,
            p.ty.variant(),
            p.id
        ));
    }
    s.push_str("            _ => None,\n        }\n    }\n\n");

    s.push_str(
        "    /// Sets the parameter `id`. Returns false (and changes nothing) for an unknown id, a value of the\n",
    );
    s.push_str("    /// wrong type or an enum index out of range. Values outside the soft range are accepted.\n");
    s.push_str("    pub fn set(&mut self, id: &str, value: ParamValue) -> bool {\n        match (id, value) {\n");
    for p in &t.params {
        let guard = if p.ty == Type::Enum {
            format!(" if v < {}", p.values.len())
        } else {
            String::new()
        };
        s.push_str(&format!(
            "            ({:?}, ParamValue::{}(v)){guard} => self.{} = v,\n",
            p.id,
            p.ty.variant(),
            p.id
        ));
    }
    s.push_str("            _ => return false,\n        }\n        true\n    }\n}\n");
    s
}

/// GLSL header: enum constants and the `Params` uniform block (instance `params`). The descriptor set and
/// binding default to 0 and can be overridden by defining `UCRT_PARAMS_SET` and `UCRT_PARAMS_BINDING` before
/// the include.
pub fn glsl_source(t: &Table) -> String {
    let mut s = String::new();
    s.push_str("// Generated by crates/paramgen from params/*.toml. Do not edit: change the TOML files.\n");
    s.push_str("#ifndef UCRT_PARAMS_GLSL\n#define UCRT_PARAMS_GLSL\n\n");
    s.push_str("#ifndef UCRT_PARAMS_SET\n#define UCRT_PARAMS_SET 0\n#endif\n");
    s.push_str("#ifndef UCRT_PARAMS_BINDING\n#define UCRT_PARAMS_BINDING 0\n#endif\n\n");
    for p in &t.params {
        for (i, v) in p.values.iter().enumerate() {
            s.push_str(&format!("const uint {} = {i}u;\n", enum_const_name(&p.id, v)));
        }
    }
    s.push_str(&format!(
        "\n// std140, {} bytes.\nlayout(std140, set = UCRT_PARAMS_SET, binding = UCRT_PARAMS_BINDING) uniform Params {{\n",
        t.size
    ));
    for p in &t.params {
        s.push_str(&format!(
            "    layout(offset = {}) {} {}; // {} [{}]\n",
            p.offset,
            p.ty.glsl(),
            p.id,
            p.unit,
            p.tag
        ));
    }
    s.push_str("} params;\n\n#endif\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, ty: &str, default: &str, extra: &str) -> String {
        format!(
            "[[param]]\nid = \"{id}\"\nstage = \"t\"\ntype = \"{ty}\"\nunit = \"1\"\ndefault = {default}\n{extra}\
             tag = \"E\"\nsource = \"test\"\ndescription = \"test entry\"\ngroup = \"Test\"\n\n"
        )
    }

    const RANGE: &str = "min = -10.0\nmax = 10.0\n";

    /// Mixed float, int, bool, enum and vector members.
    fn mixed() -> String {
        [
            entry("a", "f32", "1.0", RANGE),
            entry("b", "vec3", "[1.0, 2.0, 3.0]", RANGE),
            entry("c", "i32", "-3", "min = -10\nmax = 10\n"),
            entry("d", "vec2", "[0.5, 0.25]", RANGE),
            entry("e", "bool", "true", ""),
            entry("f", "vec4", "[1, 2, 3, 4]", RANGE),
            entry("g", "f32", "2", RANGE),
            entry("h", "enum", "\"two\"", "values = [\"one\", \"two\", \"three\"]\n"),
            entry("i", "vec2", "[0.0, 0.0]", RANGE),
            entry("j", "f32", "0.0", RANGE),
        ]
        .concat()
    }

    fn table(text: &str) -> Result<Table, Error> {
        build(&[("params/t.toml".into(), "t".into(), text.into())])
    }

    #[test]
    fn std140_offsets_of_mixed_members_match_hand_computed_values() {
        let t = table(&mixed()).unwrap();
        let offsets: Vec<(&str, usize)> = t.params.iter().map(|p| (p.id.as_str(), p.offset)).collect();
        // a float at 0; vec3 aligned to 16; an int fills the 4th slot of the vec3 (28); vec2 aligned to 8 (32);
        // bool at 40; vec4 aligned to 16 (48); float at 64; enum (uint) at 68; vec2 at 72; float at 80.
        let expected = [
            ("a", 0),
            ("b", 16),
            ("c", 28),
            ("d", 32),
            ("e", 40),
            ("f", 48),
            ("g", 64),
            ("h", 68),
            ("i", 72),
            ("j", 80),
        ];
        assert_eq!(offsets, expected);
        // 84 bytes used, block rounded up to 16.
        assert_eq!(t.size, 96);
    }

    #[test]
    fn vec3_after_vec3_starts_on_the_next_16_byte_boundary() {
        let text = [
            entry("a", "vec3", "[0, 0, 0]", RANGE),
            entry("b", "vec3", "[0, 0, 0]", RANGE),
            entry("c", "vec2", "[0, 0]", RANGE),
            entry("d", "vec2", "[0, 0]", RANGE),
        ]
        .concat();
        let t = table(&text).unwrap();
        let offsets: Vec<usize> = t.params.iter().map(|p| p.offset).collect();
        assert_eq!(offsets, [0, 16, 32, 40]);
        assert_eq!(t.size, 48);
    }

    #[test]
    fn defaults_are_parsed_per_type() {
        let t = table(&mixed()).unwrap();
        let d: Vec<&Value> = t.params.iter().map(|p| &p.default).collect();
        assert_eq!(d[0], &Value::F32(1.0));
        assert_eq!(d[1], &Value::Vec(vec![1.0, 2.0, 3.0]));
        assert_eq!(d[2], &Value::I32(-3));
        assert_eq!(d[4], &Value::Bool(true));
        assert_eq!(d[5], &Value::Vec(vec![1.0, 2.0, 3.0, 4.0]));
        assert_eq!(d[6], &Value::F32(2.0));
        assert_eq!(d[7], &Value::Enum(1));
    }

    #[test]
    fn an_added_parameter_appears_in_rust_glsl_and_metadata() {
        let before = table(&mixed()).unwrap();
        let text = mixed() + &entry("new_gain", "f32", "0.5", "min = 0.0\nmax = 1.0\n");
        let t = table(&text).unwrap();
        assert_eq!(t.params.len(), before.params.len() + 1);
        let p = t.params.last().unwrap();
        assert_eq!(p.id, "new_gain");
        assert_eq!(p.offset, 84);

        let rust = rust_source(&t);
        assert!(!rust_source(&before).contains("new_gain"));
        assert!(rust.contains("    pub new_gain: f32,\n"));
        assert!(rust.contains("            new_gain: 0.5_f32,\n"));
        assert!(rust.contains("        id: \"new_gain\",\n"));
        assert!(rust.contains("out[84..88].copy_from_slice(&self.new_gain.to_le_bytes());"));

        let glsl = glsl_source(&t);
        assert!(!glsl_source(&before).contains("new_gain"));
        assert!(glsl.contains("    layout(offset = 84) float new_gain; // 1 [E]\n"));
    }

    #[test]
    fn enum_constants_are_generated_in_both_languages() {
        let t = table(&mixed()).unwrap();
        assert!(rust_source(&t).contains("pub const H_THREE: u32 = 2;"));
        assert!(glsl_source(&t).contains("const uint H_THREE = 2u;"));
    }

    #[test]
    fn duplicate_id_names_both_files_and_the_id() {
        let a = entry("gain", "f32", "1.0", RANGE).replace("stage = \"t\"", "stage = \"a\"");
        let b = entry("gain", "f32", "1.0", RANGE).replace("stage = \"t\"", "stage = \"b\"");
        let e = build(&[
            ("params/a.toml".into(), "a".into(), a),
            ("params/b.toml".into(), "b".into(), b),
        ])
        .unwrap_err();
        assert_eq!(
            e.0,
            "params/b.toml: duplicate parameter id `gain` (first defined in params/a.toml)"
        );
    }

    #[test]
    fn out_of_range_default_names_the_file_and_the_id() {
        let e = table(&entry("gain", "f32", "12.0", RANGE)).unwrap_err();
        assert_eq!(
            e.0,
            "params/t.toml: parameter `gain`: default 12 is outside the soft range [-10, 10]"
        );
        let e = table(&entry("count", "i32", "-11", "min = -10\nmax = 10\n")).unwrap_err();
        assert!(
            e.0.starts_with("params/t.toml: parameter `count`: default -11 is outside"),
            "{e}"
        );
        let e = table(&entry("xy", "vec2", "[0.5, 11]", RANGE)).unwrap_err();
        assert!(
            e.0.starts_with("params/t.toml: parameter `xy`: default [0.5, 11.0] is outside"),
            "{e}"
        );
    }

    #[test]
    fn a_default_equal_to_a_non_representable_bound_is_in_range() {
        // 0.3 is not exact in f32: bounds are rounded like the value, so 0.3 within [0.1, 0.3] is accepted.
        assert!(table(&entry("x", "f32", "0.3", "min = 0.1\nmax = 0.3\n")).is_ok());
    }

    #[test]
    fn invalid_entries_are_rejected_with_the_file_and_the_id() {
        let cases = [
            (entry("Bad", "f32", "1.0", RANGE), "the id must be snake_case"),
            (entry("x", "f64", "1.0", RANGE), "unknown type `f64`"),
            (entry("x", "f32", "1.0", ""), "need `min` and `max`"),
            (entry("x", "f32", "1.0", "min = 2.0\nmax = 1.0\n"), "invalid soft range"),
            (entry("x", "bool", "true", RANGE), "not allowed for type bool"),
            (entry("x", "i32", "1.5", RANGE), "must be an integer"),
            (entry("x", "vec3", "[1, 2]", RANGE), "array of 3 numbers"),
            (
                entry("x", "enum", "\"z\"", "values = [\"a\", \"b\"]\n"),
                "default `z` is not one of",
            ),
            (entry("x", "enum", "\"a\"", "values = [\"a\", \"a\"]\n"), "listed twice"),
            (entry("x", "enum", "\"a\"", ""), "non-empty `values`"),
            (
                entry("x", "f32", "1.0", RANGE).replace("tag = \"E\"", "tag = \"X\""),
                "must be one of S, D, K, E",
            ),
            (
                entry("x", "f32", "1.0", RANGE).replace("stage = \"t\"", "stage = \"u\""),
                "does not match",
            ),
            (
                entry("x", "f32", "1.0", RANGE).replace("source = \"test\"", "source = \" \""),
                "`source` is empty",
            ),
        ];
        for (text, needle) in cases {
            let e = table(&text).unwrap_err();
            assert!(e.0.starts_with("params/t.toml: parameter `"), "{e}");
            assert!(e.0.contains(needle), "expected `{needle}` in: {e}");
        }
    }

    #[test]
    fn unknown_keys_and_syntax_errors_name_the_file() {
        let e = table(&entry("x", "f32", "1.0", "min = 0\nmax = 2\nmaximum = 3\n")).unwrap_err();
        assert!(e.0.starts_with("params/t.toml: "), "{e}");
        assert!(e.0.contains("maximum"), "{e}");
        let e = table("[[param]\n").unwrap_err();
        assert!(e.0.starts_with("params/t.toml: "), "{e}");
    }

    #[test]
    fn empty_table_is_an_error() {
        assert!(table("").is_err());
    }

    fn glslc() -> std::path::PathBuf {
        if let Ok(sdk) = std::env::var("VULKAN_SDK") {
            let p = std::path::PathBuf::from(sdk).join("Bin").join("glslc.exe");
            if p.exists() {
                return p;
            }
        }
        "glslc".into()
    }

    /// Compiles a compute shader that includes the generated header and reads every member.
    fn compile_with_glslc(t: &Table, name: &str) -> std::process::Output {
        let dir = std::env::temp_dir().join(format!("paramgen-test-{}-{name}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("params.glsl"), glsl_source(t)).unwrap();
        let mut reads = String::new();
        for p in &t.params {
            let x = match p.ty {
                Type::F32 => format!("params.{}", p.id),
                Type::I32 | Type::Enum => format!("float(params.{})", p.id),
                Type::Bool => format!("(params.{} ? 1.0 : 0.0)", p.id),
                _ => format!("params.{}.x", p.id),
            };
            reads.push_str(&format!("    s += {x};\n"));
        }
        let shader = format!(
            "#version 450\n#extension GL_GOOGLE_include_directive : require\n#include \"params.glsl\"\n\
             layout(local_size_x = 1) in;\nlayout(std430, set = 0, binding = 1) buffer Out {{ float v; }} o;\n\
             void main() {{\n    float s = 0.0;\n{reads}    o.v = s;\n}}\n"
        );
        fs::write(dir.join("check.comp"), shader).unwrap();
        let out = std::process::Command::new(glslc())
            .args(["--target-env=vulkan1.3", "-o"])
            .arg(dir.join("check.spv"))
            .arg(dir.join("check.comp"))
            .output()
            .expect("cannot run glslc; install the Vulkan SDK and set VULKAN_SDK");
        let _ = fs::remove_dir_all(&dir);
        out
    }

    #[test]
    fn generated_header_compiles_with_glslc() {
        let out = compile_with_glslc(&table(&mixed()).unwrap(), "ok");
        assert!(
            out.status.success(),
            "glslc failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn glslc_rejects_a_misaligned_offset() {
        // Guards the claim that glslc checks explicit offsets: a vec4 at 52 (not 16-aligned) must not compile.
        let mut t = table(&mixed()).unwrap();
        t.params[5].offset = 52;
        let out = compile_with_glslc(&t, "bad");
        assert!(!out.status.success());
    }

    #[test]
    fn repository_table_loads_and_test_files_come_last() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("params");
        let release = load_dir(&dir, false).unwrap();
        let debug = load_dir(&dir, true).unwrap();
        assert!(release.params.iter().all(|p| !p.file.starts_with("params/_")));
        assert!(debug.params.iter().any(|p| p.file == "params/_test.toml"));
        // Production entries keep their offsets whether or not the test entries are loaded.
        assert_eq!(release.params[..], debug.params[..release.params.len()]);
        assert!(release.params.iter().any(|p| p.id == "exposure"));
    }
}
