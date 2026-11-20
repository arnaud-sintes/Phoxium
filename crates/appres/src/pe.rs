//! Minimal reader of the resource directory of a PE32+ executable (tests only need ordinal-named resources).
//!
//! DOS header -> `e_lfanew` -> "PE\0\0", COFF header (section count, optional header size), optional header
//! (PE32+ magic 0x20B, data directory 2 = resources), section table (RVA to file offset). The resource tree has
//! three levels (type, name, language) of IMAGE_RESOURCE_DIRECTORY (16 bytes, then named entries, then id
//! entries, 8 bytes each; high bit of the offset = subdirectory), leaves are IMAGE_RESOURCE_DATA_ENTRY
//! (data RVA, size).

use crate::{u16_at, u32_at};

/// One resource: type, name and language (ordinals; a string name is reported as `None`), and its data.
#[derive(Clone, Debug)]
pub struct Resource {
    pub kind: Option<u16>,
    pub name: Option<u16>,
    pub lang: u16,
    pub data: Vec<u8>,
}

struct Section {
    va: u32,
    size: u32,
    raw: u32,
}

/// Every resource of the PE image `file`.
pub fn resources(file: &[u8]) -> Result<Vec<Resource>, String> {
    let bad = |what: &str| format!("not a valid PE32+ image: {what}");
    if file.get(0..2) != Some(b"MZ") {
        return Err(bad("no MZ header"));
    }
    let pe = u32_at(file, 0x3C).ok_or_else(|| bad("truncated"))? as usize;
    if file.get(pe..pe + 4) != Some(b"PE\0\0") {
        return Err(bad("no PE signature"));
    }
    let coff = pe + 4;
    let sections = u16_at(file, coff + 2).ok_or_else(|| bad("truncated"))? as usize;
    let opt_size = u16_at(file, coff + 16).ok_or_else(|| bad("truncated"))? as usize;
    let opt = coff + 20;
    if u16_at(file, opt) != Some(0x20B) {
        return Err(bad("not PE32+"));
    }
    // PE32+: data directories start at offset 112 of the optional header, 8 bytes each.
    let rsrc_rva = u32_at(file, opt + 112 + 2 * 8).ok_or_else(|| bad("truncated"))?;
    let table = opt + opt_size;
    let secs: Vec<Section> = (0..sections)
        .map(|i| {
            let s = table + 40 * i;
            Some(Section {
                size: u32_at(file, s + 8)?.max(u32_at(file, s + 16)?),
                va: u32_at(file, s + 12)?,
                raw: u32_at(file, s + 20)?,
            })
        })
        .collect::<Option<_>>()
        .ok_or_else(|| bad("truncated section table"))?;
    if rsrc_rva == 0 {
        return Ok(Vec::new());
    }
    let to_off = |rva: u32| -> Option<usize> {
        secs.iter()
            .find(|s| rva >= s.va && rva < s.va + s.size)
            .map(|s| (rva - s.va + s.raw) as usize)
    };
    let base = to_off(rsrc_rva).ok_or_else(|| bad("resource RVA outside the sections"))?;
    let mut out = Vec::new();
    let entries = |dir: usize| -> Option<Vec<(Option<u16>, u32)>> {
        let at = base + dir;
        let n = u16_at(file, at + 12)? as usize + u16_at(file, at + 14)? as usize;
        (0..n)
            .map(|i| {
                let e = at + 16 + 8 * i;
                let id = u32_at(file, e)?;
                let name = (id & 0x8000_0000 == 0).then_some(id as u16);
                Some((name, u32_at(file, e + 4)?))
            })
            .collect()
    };
    let sub = |off: u32| (off & 0x8000_0000 != 0).then_some((off & 0x7FFF_FFFF) as usize);
    let mut walk = || -> Option<()> {
        for (kind, o1) in entries(0)? {
            for (name, o2) in entries(sub(o1)?)? {
                for (lang, o3) in entries(sub(o2)?)? {
                    if sub(o3).is_some() {
                        return None;
                    }
                    let leaf = base + o3 as usize;
                    let rva = u32_at(file, leaf)?;
                    let size = u32_at(file, leaf + 4)? as usize;
                    let at = to_off(rva)?;
                    out.push(Resource {
                        kind,
                        name,
                        lang: lang.unwrap_or(0),
                        data: file.get(at..at + size)?.to_vec(),
                    });
                }
            }
        }
        Some(())
    };
    walk().ok_or_else(|| bad("malformed resource directory"))?;
    Ok(out)
}

/// Checks that the PE image `exe` embeds the `.ico` file `ico` as icon group 1 (every frame, same order, same
/// bytes, each RT_ICON present) and a VERSIONINFO containing every string of `strings`. Returns the sides of
/// the embedded frames.
pub fn check_executable(exe: &[u8], ico: &[u8], strings: &[&str]) -> Result<Vec<u32>, String> {
    use crate::rt;
    let all = resources(exe)?;
    let frames = crate::ico::parse(ico)?;
    let find = |kind: u16, name: u16| all.iter().find(|r| r.kind == Some(kind) && r.name == Some(name));
    let group = find(rt::GROUP_ICON, 1).ok_or("no RT_GROUP_ICON 1 in the executable")?;
    let entries = group_entries(&group.data)?;
    if entries.len() != frames.len() {
        return Err(format!(
            "{} icon frames embedded, {} in the .ico",
            entries.len(),
            frames.len()
        ));
    }
    for (e, f) in entries.iter().zip(&frames) {
        let icon = find(rt::ICON, e.icon_id).ok_or(format!("RT_ICON {} missing", e.icon_id))?;
        if e.size != f.size || e.bit_count != 32 || e.bytes as usize != f.data.len() || icon.data != f.data {
            return Err(format!("frame {} px differs from the .ico", f.size));
        }
    }
    let version = find(rt::VERSION, 1).ok_or("no VERSIONINFO in the executable")?;
    for s in strings {
        if !crate::contains_utf16(&version.data, s) {
            return Err(format!("VERSIONINFO lacks \"{s}\""));
        }
    }
    Ok(entries.iter().map(|e| e.size).collect())
}

/// One entry of an RT_GROUP_ICON: the side in pixels (0 read as 256), bit count, byte size and RT_ICON id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GroupEntry {
    pub size: u32,
    pub bit_count: u16,
    pub bytes: u32,
    pub icon_id: u16,
}

/// Parses the data of an RT_GROUP_ICON resource.
pub fn group_entries(data: &[u8]) -> Result<Vec<GroupEntry>, String> {
    let bad = || "malformed RT_GROUP_ICON".to_string();
    if u16_at(data, 0) != Some(0) || u16_at(data, 2) != Some(1) {
        return Err(bad());
    }
    let n = u16_at(data, 4).ok_or_else(bad)? as usize;
    (0..n)
        .map(|i| {
            let e = 6 + 14 * i;
            let w = *data.get(e).ok_or_else(bad)?;
            Ok(GroupEntry {
                size: if w == 0 { 256 } else { w as u32 },
                bit_count: u16_at(data, e + 6).ok_or_else(bad)?,
                bytes: u32_at(data, e + 8).ok_or_else(bad)?,
                icon_id: u16_at(data, e + 12).ok_or_else(bad)?,
            })
        })
        .collect()
}
