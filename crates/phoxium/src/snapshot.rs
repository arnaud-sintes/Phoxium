//! Snapshot of the output (TODO "live snapshot"): the picture the CRT chain drew, saved as a PNG file or copied to the
//! clipboard, without the menu on it.
//!
//! The renderer copies the presented swapchain image to the host ([`crate::gpu::Renderer::request_snapshot`]); this
//! module turns that [`SnapshotImage`] into a PNG (8-bit RGB, alpha dropped), picks the file name
//! (`%USERPROFILE%\Pictures\Phoxium\Phoxium-YYYYMMDD-HHMMSS.png`, UTC, a `-2`, `-3` suffix when the name exists) and
//! builds the device-independent bitmap the clipboard takes (`CF_DIB`: a 40-byte header, 32 bits per pixel, rows
//! bottom to top). The pixels are the display codes of the output: what is on the screen, dithering included.

use std::path::{Path, PathBuf};

use crate::gpu::SnapshotImage;
use crate::Res;

/// What to do with a snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Write a PNG file in the pictures folder.
    Save,
    /// Put the picture on the clipboard (only on an explicit request: it replaces what the clipboard holds).
    Copy,
}

/// The folder snapshots are saved in: `%USERPROFILE%\Pictures\Phoxium`, or a `Phoxium` folder in the temporary
/// folder when there is no user profile.
pub fn pictures_dir() -> PathBuf {
    match std::env::var_os("USERPROFILE") {
        Some(p) if !p.is_empty() => PathBuf::from(p).join("Pictures").join("Phoxium"),
        _ => std::env::temp_dir().join("Phoxium"),
    }
}

/// `Phoxium-YYYYMMDD-HHMMSS.png` for `unix_secs` (UTC).
pub fn file_name(unix_secs: u64) -> String {
    let (y, mo, d, h, mi, s) = crate::crash::utc(unix_secs);
    format!("Phoxium-{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}.png")
}

/// `dir\name`, or `dir\<stem>-2.png`, `-3`... when that file exists already (a second snapshot in the same second).
pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let stem = name.strip_suffix(".png").unwrap_or(name);
    (2..)
        .map(|n| dir.join(format!("{stem}-{n}.png")))
        .find(|p| !p.exists())
        .expect("an unbounded range")
}

/// The picture as an 8-bit RGB PNG.
pub fn encode_png(img: &SnapshotImage) -> Res<Vec<u8>> {
    if img.rgba.len() != img.width as usize * img.height as usize * 4 {
        return Err("the snapshot has the wrong size".into());
    }
    let mut rgb = Vec::with_capacity(img.width as usize * img.height as usize * 3);
    for p in img.rgba.as_chunks::<4>().0 {
        rgb.extend_from_slice(&p[..3]);
    }
    let mut bytes = Vec::new();
    let mut enc = png::Encoder::new(&mut bytes, img.width, img.height);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(&rgb)?;
    Ok(bytes)
}

/// Writes the PNG of `img` to a new file in `dir` (created when missing) named for `unix_secs`; returns its path.
pub fn save(img: &SnapshotImage, dir: &Path, unix_secs: u64) -> Res<PathBuf> {
    let bytes = encode_png(img)?;
    std::fs::create_dir_all(dir)?;
    let path = unique_path(dir, &file_name(unix_secs));
    std::fs::write(&path, bytes)?;
    Ok(path)
}

/// Writes the PNG of `img` to exactly `path` (`--snapshot <png>`).
pub fn save_to(img: &SnapshotImage, path: &Path) -> Res<()> {
    std::fs::write(path, encode_png(img)?)?;
    Ok(())
}

/// The clipboard bitmap (`CF_DIB`): `BITMAPINFOHEADER` (40 bytes, 32 bits per pixel, no compression) followed by the
/// rows bottom to top as B, G, R, unused.
pub fn dib(img: &SnapshotImage) -> Vec<u8> {
    let (w, h) = (img.width as usize, img.height as usize);
    let mut out = Vec::with_capacity(40 + w * h * 4);
    out.extend_from_slice(&40u32.to_le_bytes()); // biSize
    out.extend_from_slice(&(img.width as i32).to_le_bytes()); // biWidth
    out.extend_from_slice(&(img.height as i32).to_le_bytes()); // biHeight, positive: bottom-up
    out.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    out.extend_from_slice(&32u16.to_le_bytes()); // biBitCount
    out.extend_from_slice(&0u32.to_le_bytes()); // biCompression: BI_RGB
    out.extend_from_slice(&((w * h * 4) as u32).to_le_bytes()); // biSizeImage
    out.extend_from_slice(&[0; 16]); // pixels per metre x and y, colours used and important
    for y in (0..h).rev() {
        for p in img.rgba[y * w * 4..(y + 1) * w * 4].as_chunks::<4>().0 {
            out.extend_from_slice(&[p[2], p[1], p[0], 0]);
        }
    }
    out
}

/// Does what `kind` asks with `img`: saves a PNG in [`pictures_dir`] or copies it to the clipboard (owned by `owner`, the
/// output window). Returns whether it worked and the text for the menu and the console (the file, or the error).
pub fn deliver(kind: Kind, img: &SnapshotImage, owner: isize) -> (bool, String) {
    match kind {
        Kind::Save => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            match save(img, &pictures_dir(), now) {
                Ok(path) => (true, format!("Saved {}", path.display())),
                Err(e) => (false, format!("Snapshot not saved: {e}")),
            }
        }
        Kind::Copy => match crate::shell::copy_dib(owner, &dib(img)) {
            Ok(()) => (true, "Snapshot copied to the clipboard".to_string()),
            Err(e) => (false, format!("Snapshot not copied: {e}")),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image() -> SnapshotImage {
        // 2 x 2: red, green / blue, white.
        SnapshotImage {
            width: 2,
            height: 2,
            rgba: vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255],
        }
    }

    #[test]
    fn the_file_name_is_the_utc_time() {
        // 2026-11-23 17:05:09 UTC.
        assert_eq!(file_name(1_795_453_509), "Phoxium-20261123-170509.png");
    }

    #[test]
    fn an_existing_name_gets_a_suffix() {
        let dir = std::env::temp_dir().join(format!("phoxium-snap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let name = "Phoxium-20260101-000000.png";
        assert_eq!(unique_path(&dir, name), dir.join(name));
        std::fs::write(dir.join(name), b"x").unwrap();
        assert_eq!(unique_path(&dir, name), dir.join("Phoxium-20260101-000000-2.png"));
        std::fs::write(dir.join("Phoxium-20260101-000000-2.png"), b"x").unwrap();
        assert_eq!(unique_path(&dir, name), dir.join("Phoxium-20260101-000000-3.png"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_png_round_trips_the_pixels() {
        let dir = std::env::temp_dir().join(format!("phoxium-snap-rt-{}", std::process::id()));
        let img = image();
        let path = save(&img, &dir, 1_795_453_509).unwrap();
        assert_eq!(path.file_name().unwrap(), "Phoxium-20261123-170509.png");
        let decoder = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(&path).unwrap()));
        let mut reader = decoder.read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        assert_eq!((info.width, info.height, info.color_type), (2, 2, png::ColorType::Rgb));
        assert_eq!(&buf[..12], &[255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_wrong_sized_snapshot_is_an_error() {
        let mut img = image();
        img.rgba.pop();
        assert!(encode_png(&img).is_err());
    }

    #[test]
    fn the_clipboard_bitmap_is_bottom_up_bgr() {
        let d = dib(&image());
        assert_eq!(u32::from_le_bytes(d[0..4].try_into().unwrap()), 40);
        assert_eq!(i32::from_le_bytes(d[4..8].try_into().unwrap()), 2);
        assert_eq!(i32::from_le_bytes(d[8..12].try_into().unwrap()), 2);
        assert_eq!(u16::from_le_bytes(d[14..16].try_into().unwrap()), 32);
        assert_eq!(d.len(), 40 + 16);
        // The bottom row (blue, white) first, as B, G, R, 0.
        assert_eq!(&d[40..48], &[255, 0, 0, 0, 255, 255, 255, 0]);
        // Then the top row (red, green).
        assert_eq!(&d[48..56], &[0, 0, 255, 0, 0, 255, 0, 0]);
    }
}
