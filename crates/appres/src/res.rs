//! Win32 `.res` files (the output format of rc.exe), written directly.
//!
//! A `.res` file is a sequence of resources, each a RESOURCEHEADER (data size, header size, type, name, data
//! version, memory flags, language, version, characteristics; type and name here are ordinals 0xFFFF + id)
//! followed by its data, every item aligned to 4 bytes. It starts with an empty resource (type 0, name 0) that
//! marks the 32-bit format. The MSVC linker takes the file as an input and converts it (CVTRES).
//!
//! An icon group is stored as one RT_ICON resource per frame (the frame bytes of the `.ico`, unchanged) plus
//! an RT_GROUP_ICON whose entries are the ICONDIRENTRY fields with the 4-byte file offset replaced by the
//! 2-byte RT_ICON id. The lowest group id is the icon File Explorer shows for the executable.

use crate::{ico::Frame, rt, utf16_bytes};

/// US English, the language of every resource written here.
pub const LANG_EN_US: u16 = 0x0409;
/// Unicode code page of the VERSIONINFO string table (`040904b0`).
const CODEPAGE_UNICODE: u16 = 0x04B0;

const MOVEABLE: u16 = 0x0010;
const PURE: u16 = 0x0020;
const DISCARDABLE: u16 = 0x1000;

/// Builder of a `.res` file.
pub struct ResFile {
    bytes: Vec<u8>,
    next_icon_id: u16,
}

impl Default for ResFile {
    fn default() -> Self {
        Self::new()
    }
}

impl ResFile {
    pub fn new() -> Self {
        let mut r = ResFile {
            bytes: Vec::new(),
            next_icon_id: 1,
        };
        r.push(0, 0, 0, 0, &[]);
        r
    }

    fn push(&mut self, kind: u16, name: u16, flags: u16, lang: u16, data: &[u8]) {
        let b = &mut self.bytes;
        b.extend_from_slice(&(data.len() as u32).to_le_bytes());
        b.extend_from_slice(&32u32.to_le_bytes());
        for ord in [kind, name] {
            b.extend_from_slice(&0xFFFFu16.to_le_bytes());
            b.extend_from_slice(&ord.to_le_bytes());
        }
        b.extend_from_slice(&0u32.to_le_bytes()); // DataVersion
        b.extend_from_slice(&flags.to_le_bytes());
        b.extend_from_slice(&lang.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes()); // Version
        b.extend_from_slice(&0u32.to_le_bytes()); // Characteristics
        b.extend_from_slice(data);
        pad4(b);
    }

    /// Adds the frames of an `.ico` as RT_ICON resources (fresh ids) and an RT_GROUP_ICON `group_id` naming them.
    pub fn icon_group(&mut self, group_id: u16, frames: &[Frame]) -> &mut Self {
        let mut group = Vec::new();
        group.extend_from_slice(&0u16.to_le_bytes());
        group.extend_from_slice(&1u16.to_le_bytes());
        group.extend_from_slice(&(frames.len() as u16).to_le_bytes());
        for f in frames {
            let id = self.next_icon_id;
            self.next_icon_id += 1;
            self.push(rt::ICON, id, MOVEABLE | DISCARDABLE, LANG_EN_US, &f.data);
            group.extend_from_slice(&f.dir_fields());
            group.extend_from_slice(&id.to_le_bytes());
        }
        self.push(
            rt::GROUP_ICON,
            group_id,
            MOVEABLE | PURE | DISCARDABLE,
            LANG_EN_US,
            &group,
        );
        self
    }

    /// Adds the VERSIONINFO resource (id 1).
    pub fn version_info(&mut self, v: &VersionInfo) -> &mut Self {
        let data = v.encode();
        self.push(rt::VERSION, 1, MOVEABLE | PURE, LANG_EN_US, &data);
        self
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

fn pad4(b: &mut Vec<u8>) {
    while !b.len().is_multiple_of(4) {
        b.push(0);
    }
}

/// The content of a VERSIONINFO resource.
#[derive(Clone, Debug)]
pub struct VersionInfo {
    /// major, minor, patch, build.
    pub version: [u16; 4],
    /// Name/value pairs of the `040904b0` string table (FileDescription, ProductName, ...).
    pub strings: Vec<(String, String)>,
}

impl VersionInfo {
    /// Parses `major.minor.patch` (a pre-release or build suffix is ignored) into a four-part version.
    pub fn parse_version(s: &str) -> [u16; 4] {
        let core = s.split(['-', '+']).next().unwrap_or("");
        let mut v = [0u16; 4];
        for (slot, part) in v.iter_mut().zip(core.split('.')) {
            *slot = part.parse().unwrap_or(0);
        }
        v
    }

    /// VS_VERSIONINFO with its VS_FIXEDFILEINFO, a StringFileInfo (one table) and a VarFileInfo (Translation).
    pub fn encode(&self) -> Vec<u8> {
        let [a, b, c, d] = self.version.map(u32::from);
        let ms = (a << 16) | b;
        let ls = (c << 16) | d;
        let mut fixed = Vec::new();
        for x in [
            0xFEEF_04BD, // dwSignature
            0x0001_0000, // dwStrucVersion
            ms,
            ls, // file version
            ms,
            ls,          // product version
            0x3F,        // dwFileFlagsMask
            0,           // dwFileFlags
            0x0004_0004, // dwFileOS = VOS_NT_WINDOWS32
            1,           // dwFileType = VFT_APP
            0,           // dwFileSubtype
            0,
            0, // file date
        ] {
            fixed.extend_from_slice(&u32::to_le_bytes(x));
        }
        let strings: Vec<Vec<u8>> = self
            .strings
            .iter()
            .map(|(k, v)| {
                let mut value = utf16_bytes(v);
                value.extend_from_slice(&[0, 0]);
                node(k, &value, (v.encode_utf16().count() + 1) as u16, 1, &[])
            })
            .collect();
        let table = node(&format!("{LANG_EN_US:04x}{CODEPAGE_UNICODE:04x}"), &[], 0, 1, &strings);
        let sfi = node("StringFileInfo", &[], 0, 1, &[table]);
        let translation = ((CODEPAGE_UNICODE as u32) << 16) | LANG_EN_US as u32;
        let var = node("Translation", &translation.to_le_bytes(), 4, 0, &[]);
        let vfi = node("VarFileInfo", &[], 0, 1, &[var]);
        node("VS_VERSION_INFO", &fixed, fixed.len() as u16, 0, &[sfi, vfi])
    }
}

/// One VERSIONINFO block: wLength, wValueLength, wType, the key (UTF-16, terminated), padding, the value,
/// padding, then the children, each starting on a 4-byte boundary. wLength excludes the trailing padding.
fn node(key: &str, value: &[u8], value_len: u16, kind: u16, children: &[Vec<u8>]) -> Vec<u8> {
    let mut b = vec![0u8; 4];
    b.extend_from_slice(&kind.to_le_bytes());
    b.extend_from_slice(&utf16_bytes(key));
    b.extend_from_slice(&[0, 0]);
    pad4(&mut b);
    b.extend_from_slice(value);
    for c in children {
        pad4(&mut b);
        b.extend_from_slice(c);
    }
    let len = b.len() as u16;
    b[0..2].copy_from_slice(&len.to_le_bytes());
    b[2..4].copy_from_slice(&value_len.to_le_bytes());
    b
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{contains_utf16, u16_at, u32_at};

    #[test]
    fn version_parse() {
        assert_eq!(VersionInfo::parse_version("0.1.0"), [0, 1, 0, 0]);
        assert_eq!(VersionInfo::parse_version("1.22.3-beta.1"), [1, 22, 3, 0]);
    }

    #[test]
    fn version_info_layout() {
        let v = VersionInfo {
            version: [1, 2, 3, 0],
            strings: vec![("ProductName".into(), "ultimateCRT".into())],
        };
        let d = v.encode();
        assert_eq!(u16_at(&d, 0), Some(d.len() as u16));
        assert_eq!(u16_at(&d, 2), Some(52));
        assert!(contains_utf16(&d, "VS_VERSION_INFO"));
        // Key "VS_VERSION_INFO" + terminator = 32 bytes after the 6-byte header, padded to 40.
        assert_eq!(u32_at(&d, 40), Some(0xFEEF_04BD));
        assert_eq!(u32_at(&d, 48), Some((1 << 16) | 2));
        assert_eq!(u32_at(&d, 52), Some(3 << 16));
        assert!(contains_utf16(&d, "040904b0"));
        assert!(contains_utf16(&d, "ultimateCRT"));
    }

    #[test]
    fn res_records() {
        let frames = [
            Frame {
                size: 16,
                data: vec![1; 10],
            },
            Frame {
                size: 256,
                data: vec![2; 7],
            },
        ];
        let mut r = ResFile::new();
        r.icon_group(1, &frames);
        let b = r.into_bytes();
        // Empty first record, then two icons and the group, each 4-byte aligned.
        assert_eq!(&b[0..8], &[0, 0, 0, 0, 32, 0, 0, 0]);
        let mut at = 32;
        let mut seen = Vec::new();
        while at < b.len() {
            let size = u32_at(&b, at).unwrap() as usize;
            let kind = u16_at(&b, at + 10).unwrap();
            let name = u16_at(&b, at + 14).unwrap();
            seen.push((kind, name, size));
            at += 32 + size.div_ceil(4) * 4;
        }
        assert_eq!(at, b.len());
        assert_eq!(seen, vec![(3, 1, 10), (3, 2, 7), (14, 1, 6 + 2 * 14)]);
        // Group entry 2: side 0 (256), RT_ICON id 2.
        let g = b.len() - 6 - 2 * 14 - 2; // group data starts here (28 + 6 bytes, padded by 2)
        assert_eq!(b[g + 6 + 14], 0);
        assert_eq!(u16_at(&b, g + 6 + 14 + 12), Some(2));
    }
}
