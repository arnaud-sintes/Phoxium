//! The persistent tube state (T-43, QUESTIONS PHY-14): the burn-in wear map saved between sessions, per tube
//! preset, and the controller that loads it into the renderer, saves it periodically and at exit.
//!
//! # File
//!
//! `%APPDATA%\ultimateCRT\tubes\<preset>.state` (next to `settings.toml`, `settings::default_dir`; `default`
//! when no preset is in force; the id is reduced to `a-z 0-9 - _`). Little endian:
//!
//! | Offset | Size | Field |
//! |---|---|---|
//! | 0 | 8 | magic `UCRTWEAR` |
//! | 8 | 4 | schema version, u32 ([`VERSION`]) |
//! | 12 | 4 | width, u32 (texels) |
//! | 16 | 4 | height, u32 |
//! | 20 | 4 | channels, u32 (3) |
//! | 24 | 8 | reserved, 0 |
//! | 32 | 12 w h | charge per texel and channel, f32 (reference hours of full white at 350 cd/m2), rows top to bottom |
//! | end | 4 | CRC-32 (IEEE) of every byte before it |
//!
//! Written atomically (`.tmp` in the same folder, flushed, renamed over the old file). A missing file is a new
//! tube; a file with a wrong magic, version, size or checksum, or with a negative or non-finite charge, is a new
//! tube plus a warning, and is kept as `<preset>.state.bak`; never a panic. A valid file of another map size
//! is resampled ([`crt_model::burnin::WearMap::resample`]).
//!
//! # Controller
//!
//! [`TubeState`] runs on the render thread once per frame ([`TubeState::tick`]). While burn-in is on it makes
//! sure the map of the preset in force is on the GPU (loaded at the first frame, swapped when the preset
//! changes: the old map is read back and saved first), asks every [`SAVE_PERIOD`] for an asynchronous copy of
//! the map (no wait on the render thread, [`WearHost::take_wear_snapshot`]) and writes it on a background
//! thread. [`TubeState::finish`] waits for that thread, reads the map back (blocking, once) and writes it at
//! exit. `--reset-burnin` and the menu button "New tube" start from a clean map. A snapshot asked for before a
//! preset switch or a "New tube" is dropped when it arrives (a generation counter), so it never lands in
//! another preset's file nor undoes the new tube. Without a settings folder no state file is read or written:
//! the wear lasts for the session.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    thread::JoinHandle,
    time::{Duration, Instant},
};

use crate::params::Params;
use crate::Res;

/// File magic.
pub const MAGIC: &[u8; 8] = b"UCRTWEAR";
/// Schema version of the file.
pub const VERSION: u32 = 1;
const HEADER: usize = 32;
/// Time between two periodic saves.
pub const SAVE_PERIOD: Duration = Duration::from_secs(300);

/// A wear map: charge per texel and channel (rgb interleaved), reference hours, rows top to bottom.
#[derive(Clone, Debug, PartialEq)]
pub struct WearMap {
    pub width: u32,
    pub height: u32,
    pub q: Vec<f32>,
}

impl WearMap {
    /// A new tube.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            q: vec![0.0; width as usize * height as usize * 3],
        }
    }

    /// The map at `width` x `height`: itself when the size matches, else resampled bilinearly
    /// (`crt_model::burnin`).
    pub fn resampled(&self, width: u32, height: u32) -> WearMap {
        if (self.width, self.height) == (width, height) {
            return self.clone();
        }
        let src = crt_model::burnin::WearMap {
            width: self.width as usize,
            height: self.height as usize,
            q: self
                .q
                .as_chunks::<3>()
                .0
                .iter()
                .map(|c| [f64::from(c[0]), f64::from(c[1]), f64::from(c[2])])
                .collect(),
        };
        let r = src.resample(width as usize, height as usize);
        WearMap {
            width,
            height,
            q: r.q.iter().flat_map(|c| c.map(|v| v as f32)).collect(),
        }
    }

    /// The largest charge of any texel and channel (0 for a new tube).
    pub fn max_charge(&self) -> f32 {
        self.q.iter().copied().fold(0.0, f32::max)
    }
}

/// CRC-32 (IEEE 802.3, reflected, polynomial 0xEDB88320). [K]
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}

/// The file bytes of `map`.
pub fn encode(map: &WearMap) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER + map.q.len() * 4 + 4);
    out.extend_from_slice(MAGIC);
    for v in [VERSION, map.width, map.height, 3, 0, 0] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    for v in &map.q {
        out.extend_from_slice(&v.to_le_bytes());
    }
    let crc = crc32(&out);
    out.extend_from_slice(&crc.to_le_bytes());
    out
}

/// The map of file `bytes`, or why it is not a valid map.
pub fn decode(bytes: &[u8]) -> Result<WearMap, String> {
    if bytes.len() < HEADER + 4 {
        return Err(format!("too short ({} bytes)", bytes.len()));
    }
    if &bytes[..8] != MAGIC {
        return Err("not a wear map (wrong magic)".into());
    }
    let u32_at = |o: usize| u32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
    let version = u32_at(8);
    if version != VERSION {
        return Err(format!("schema version {version}, this build reads {VERSION}"));
    }
    let (width, height, channels) = (u32_at(12), u32_at(16), u32_at(20));
    if channels != 3 || width == 0 || height == 0 || width > 16384 || height > 16384 {
        return Err(format!("invalid size {width} x {height} x {channels}"));
    }
    let n = width as usize * height as usize * 3;
    if bytes.len() != HEADER + n * 4 + 4 {
        return Err(format!("{} bytes, {} expected", bytes.len(), HEADER + n * 4 + 4));
    }
    let body = &bytes[..bytes.len() - 4];
    let stored = u32_at(bytes.len() - 4);
    if crc32(body) != stored {
        return Err("checksum mismatch".into());
    }
    let q: Vec<f32> = body[HEADER..]
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    if q.iter().any(|v| !v.is_finite() || *v < 0.0) {
        return Err("negative or non-finite charge".into());
    }
    Ok(WearMap { width, height, q })
}

/// Writes `map` to `path` atomically (temporary file in the same folder, flushed, renamed).
pub fn save(path: &Path, map: &WearMap) -> Res<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(&encode(map))?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

/// What [`load`] found.
#[derive(Debug, PartialEq)]
pub enum Loaded {
    /// No file: a new tube.
    Missing,
    /// A valid map, at the wanted size; `resampled_from` is the stored size when it differed.
    Map {
        map: WearMap,
        resampled_from: Option<(u32, u32)>,
    },
    /// An unreadable or invalid file: a new tube; the warning says why and where the file was kept.
    Invalid(String),
}

/// Loads `path` at the map size `want`, see the module documentation.
pub fn load(path: &Path, want: (u32, u32)) -> Loaded {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Loaded::Missing,
        Err(e) => return Loaded::Invalid(format!("{}: {e}; a new tube is used", path.display())),
    };
    match decode(&bytes) {
        Ok(map) => {
            let from = (map.width, map.height);
            let resampled_from = (from != want).then_some(from);
            Loaded::Map {
                map: map.resampled(want.0, want.1),
                resampled_from,
            }
        }
        Err(why) => {
            let mut bak = path.as_os_str().to_owned();
            bak.push(".bak");
            let bak = PathBuf::from(bak);
            let kept = fs::copy(path, &bak).is_ok();
            Loaded::Invalid(format!(
                "{} is invalid ({why}); a new tube is used{}",
                path.display(),
                if kept {
                    format!(", the file was kept as {}", bak.display())
                } else {
                    String::new()
                }
            ))
        }
    }
}

/// `dir\tubes\<preset>.state`; `default` without a preset; the id reduced to `a-z 0-9 - _`.
pub fn state_path(dir: &Path, preset: Option<&str>) -> PathBuf {
    let id: String = preset
        .unwrap_or("default")
        .chars()
        .map(|c| c.to_ascii_lowercase())
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    let id = if id.is_empty() { "default".to_string() } else { id };
    dir.join("tubes").join(format!("{id}.state"))
}

/// What the controller needs from a renderer (the window `gpu::Renderer`, the headless one in tests).
pub trait WearHost {
    /// Size of the renderer's wear map, (width, height) texels.
    fn wear_map_size(&self) -> (u32, u32);
    /// Replaces the wear map (resampled to [`WearHost::wear_map_size`] by the caller).
    fn set_wear_map(&mut self, map: &WearMap) -> Res<()>;
    /// A new tube.
    fn reset_wear(&mut self);
    /// Wears the next frame's picture for `hours` at once.
    fn burn_in_now(&mut self, hours: f32);
    /// Asks for an asynchronous copy of the map.
    fn request_wear_snapshot(&mut self);
    /// The copy asked for, once ready (never waits).
    fn take_wear_snapshot(&mut self) -> Res<Option<WearMap>>;
    /// The map now (blocking), `None` before it exists.
    fn read_wear_map(&mut self) -> Res<Option<WearMap>>;
}

/// The burn-in controller of the live application, see the module documentation.
pub struct TubeState {
    /// Settings folder (`None`: nothing is saved, the wear lives for the session).
    dir: Option<PathBuf>,
    /// The state file of the map on the GPU, once loaded.
    loaded: Option<PathBuf>,
    /// `--reset-burnin`: the first map is a new tube.
    reset_first: bool,
    last_save: Instant,
    /// The map generation: bumped when the map on the GPU stops being the one of `loaded` (another preset
    /// loaded, a "New tube"). R-06.
    generation: u64,
    /// The generation a snapshot was requested in, while it is in flight. A snapshot that arrives after the
    /// generation changed is a copy of a map that is no longer the loaded one: it is dropped, never saved.
    waiting_snapshot: Option<u64>,
    writer: Option<JoinHandle<Result<(), String>>>,
    /// Messages for the console (load, save, warnings), taken by the app.
    messages: Vec<String>,
}

impl TubeState {
    pub fn new(dir: Option<PathBuf>, reset_first: bool, now: Instant) -> Self {
        Self {
            dir,
            loaded: None,
            reset_first,
            last_save: now,
            generation: 0,
            waiting_snapshot: None,
            writer: None,
            messages: Vec::new(),
        }
    }

    /// The messages since the last call (one line each).
    pub fn take_messages(&mut self) -> Vec<String> {
        std::mem::take(&mut self.messages)
    }

    /// The state file in use, once a map is loaded.
    pub fn loaded_path(&self) -> Option<&Path> {
        self.loaded.as_deref()
    }

    /// The state file of `preset`. Without a settings folder: a key only (`<preset>.state`, never a path in the
    /// current directory), which is never read or written (R-54).
    fn path_for(&self, preset: Option<&str>) -> PathBuf {
        match &self.dir {
            Some(d) => state_path(d, preset),
            None => PathBuf::from(format!("{}.state", preset.unwrap_or("default"))),
        }
    }

    /// Once per frame: see the module documentation. `preset` is the preset in force.
    pub fn tick(&mut self, host: &mut dyn WearHost, params: &Params, preset: Option<&str>, now: Instant) {
        self.collect_writer();
        if let Some(asked_in) = self.waiting_snapshot {
            match host.take_wear_snapshot() {
                Ok(Some(map)) => {
                    self.waiting_snapshot = None;
                    if asked_in != self.generation {
                        // Taken before a preset switch or a "New tube": it belongs to no file now.
                    } else if let Some(path) = self.loaded.clone() {
                        self.spawn_save(path, map);
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    self.waiting_snapshot = None;
                    self.messages.push(format!("warning: burn-in: snapshot failed: {e}"));
                }
            }
        }
        if !params.burnin_enable {
            return;
        }
        let path = self.path_for(preset);
        if self.loaded.as_deref() != Some(path.as_path()) {
            if self.loaded.is_some() {
                // Another preset: the map on the GPU belongs to the old one.
                self.save_now(host);
            }
            self.generation += 1;
            self.load_into(host, &path);
            self.loaded = Some(path);
            self.last_save = now;
        }
        if self.waiting_snapshot.is_none() && self.writer.is_none() && now.duration_since(self.last_save) >= SAVE_PERIOD
        {
            host.request_wear_snapshot();
            self.waiting_snapshot = Some(self.generation);
            self.last_save = now;
        }
    }

    fn load_into(&mut self, host: &mut dyn WearHost, path: &Path) {
        if self.dir.is_none() {
            // No settings folder: no file is read (nor kept as `.bak`) in the current directory.
            self.reset_first = false;
            host.reset_wear();
            self.messages
                .push("burn-in: new tube; no settings folder (APPDATA is not set), so the wear is not saved".into());
            return;
        }
        if std::mem::take(&mut self.reset_first) {
            host.reset_wear();
            self.messages.push(format!(
                "burn-in: new tube (--reset-burnin), saved to {}",
                path.display()
            ));
            return;
        }
        let want = host.wear_map_size();
        match load(path, want) {
            Loaded::Missing => {
                host.reset_wear();
                self.messages
                    .push(format!("burn-in: new tube, saved to {}", path.display()));
            }
            Loaded::Map { map, resampled_from } => {
                if let Some((w, h)) = resampled_from {
                    self.messages.push(format!(
                        "burn-in: {} was {w} x {h}, resampled to {} x {}",
                        path.display(),
                        want.0,
                        want.1
                    ));
                }
                match host.set_wear_map(&map) {
                    Ok(()) => self.messages.push(format!(
                        "burn-in: loaded {} (largest charge {:.1} h at 350 cd/m2)",
                        path.display(),
                        map.max_charge()
                    )),
                    Err(e) => {
                        host.reset_wear();
                        self.messages.push(format!("warning: burn-in: {e}; a new tube is used"));
                    }
                }
            }
            Loaded::Invalid(w) => {
                host.reset_wear();
                self.messages.push(format!("warning: burn-in: {w}"));
            }
        }
    }

    /// The menu's "New tube": the map is cleared now and the file replaced at the next save (at once).
    pub fn reset(&mut self, host: &mut dyn WearHost) {
        host.reset_wear();
        // A snapshot in flight holds the worn map: it must not overwrite the new tube's file.
        self.generation += 1;
        if let Some(path) = self.loaded.clone().filter(|_| self.dir.is_some()) {
            self.join_writer();
            let (w, h) = host.wear_map_size();
            self.write(&path, &WearMap::new(w, h));
            self.messages.push(format!("burn-in: new tube, {}", path.display()));
        }
    }

    /// The menu's "Burn in now".
    pub fn burn_in_now(&mut self, host: &mut dyn WearHost, hours: f32) {
        host.burn_in_now(hours);
    }

    /// Reads the map back (blocking) and writes it to the loaded state file.
    fn save_now(&mut self, host: &mut dyn WearHost) {
        self.join_writer();
        let Some(path) = self.loaded.clone() else {
            return;
        };
        match host.read_wear_map() {
            Ok(Some(map)) => self.write(&path, &map),
            Ok(None) => {}
            Err(e) => self.messages.push(format!("warning: burn-in: read back failed: {e}")),
        }
    }

    fn write(&mut self, path: &Path, map: &WearMap) {
        if self.dir.is_none() {
            return;
        }
        if let Err(e) = save(path, map) {
            self.messages
                .push(format!("warning: burn-in: cannot save {}: {e}", path.display()));
        }
    }

    fn spawn_save(&mut self, path: PathBuf, map: WearMap) {
        if self.dir.is_none() {
            return;
        }
        self.writer = Some(std::thread::spawn(move || {
            save(&path, &map).map_err(|e| format!("cannot save {}: {e}", path.display()))
        }));
    }

    fn collect_writer(&mut self) {
        if self.writer.as_ref().is_some_and(JoinHandle::is_finished) {
            self.join_writer();
        }
    }

    fn join_writer(&mut self) {
        if let Some(h) = self.writer.take() {
            match h.join() {
                Ok(Ok(())) => {}
                Ok(Err(e)) => self.messages.push(format!("warning: burn-in: {e}")),
                Err(_) => self.messages.push("warning: burn-in: the save thread panicked".into()),
            }
        }
    }

    /// At exit, on every exit path (also after a render error: a failed read back is a warning): waits for a
    /// running save, then reads the map back and writes it (when one was loaded). A snapshot still in flight
    /// is dropped: the read back is newer.
    pub fn finish(&mut self, host: &mut dyn WearHost) {
        self.waiting_snapshot = None;
        self.save_now(host);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ucrt-tube-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn sample(w: u32, h: u32) -> WearMap {
        WearMap {
            width: w,
            height: h,
            q: (0..w * h * 3).map(|i| i as f32 * 0.37 + 1e-3).collect(),
        }
    }

    /// CRC-32 check value of the standard ("123456789" -> 0xCBF43926). [K]
    #[test]
    fn crc32_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn encode_decode_round_trip_is_bit_exact() {
        let m = sample(7, 5);
        let bytes = encode(&m);
        assert_eq!(bytes.len(), HEADER + 7 * 5 * 3 * 4 + 4);
        assert_eq!(&bytes[..8], MAGIC);
        let back = decode(&bytes).unwrap();
        assert_eq!(back, m);
        assert!(back.q.iter().zip(&m.q).all(|(a, b)| a.to_bits() == b.to_bits()));
    }

    /// Corrupt, truncated, old or foreign files are refused with a reason, never a panic.
    #[test]
    fn corrupt_files_are_refused() {
        let good = encode(&sample(4, 3));
        let mut flipped = good.clone();
        flipped[HEADER + 5] ^= 0x10;
        assert!(decode(&flipped).unwrap_err().contains("checksum"));
        assert!(decode(&good[..good.len() - 1]).is_err());
        assert!(decode(&good[..10]).unwrap_err().contains("short"));
        let mut magic = good.clone();
        magic[0] = b'X';
        assert!(decode(&magic).unwrap_err().contains("magic"));
        let mut old = good.clone();
        old[8..12].copy_from_slice(&2u32.to_le_bytes());
        let n = old.len() - 4;
        let crc = crc32(&old[..n]);
        old[n..].copy_from_slice(&crc.to_le_bytes());
        assert!(decode(&old).unwrap_err().contains("version"));
        let mut neg = sample(2, 2);
        neg.q[3] = -1.0;
        assert!(decode(&encode(&neg)).unwrap_err().contains("negative"));
        neg.q[3] = f32::NAN;
        assert!(decode(&encode(&neg)).is_err());
        // Random bytes.
        let noise: Vec<u8> = (0..500u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect();
        assert!(decode(&noise).is_err());
        assert!(decode(&[]).is_err());
    }

    /// Save is atomic (no temporary file left) and loads bit-exact; a missing file is a new tube; an invalid
    /// one is a warning plus a `.bak`; another size is resampled.
    #[test]
    fn file_persistence() {
        let dir = temp_dir("persist");
        let path = state_path(&dir, Some("PVM 20M4U!"));
        assert_eq!(path, dir.join("tubes").join("pvm20m4u.state"));
        assert_eq!(load(&path, (4, 3)), Loaded::Missing);
        let m = sample(4, 3);
        save(&path, &m).unwrap();
        assert!(!path.with_extension("state.tmp").exists());
        assert_eq!(
            load(&path, (4, 3)),
            Loaded::Map {
                map: m.clone(),
                resampled_from: None
            }
        );
        // Another size: resampled, flagged.
        match load(&path, (8, 6)) {
            Loaded::Map { map, resampled_from } => {
                assert_eq!(resampled_from, Some((4, 3)));
                assert_eq!((map.width, map.height, map.q.len()), (8, 6, 8 * 6 * 3));
            }
            other => panic!("{other:?}"),
        }
        // Overwrite keeps it atomic and readable.
        let m2 = sample(4, 3);
        save(&path, &WearMap::new(4, 3)).unwrap();
        save(&path, &m2).unwrap();
        // Corrupt: a warning, the file kept as .bak, never a panic.
        fs::write(&path, b"garbage").unwrap();
        match load(&path, (4, 3)) {
            Loaded::Invalid(w) => assert!(w.contains(".bak"), "{w}"),
            other => panic!("{other:?}"),
        }
        assert!(dir.join("tubes").join("pvm20m4u.state.bak").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn state_paths() {
        let d = Path::new("C:\\x");
        assert_eq!(state_path(d, None), d.join("tubes").join("default.state"));
        assert_eq!(
            state_path(d, Some("01-pvm-20m4u")),
            d.join("tubes").join("01-pvm-20m4u.state")
        );
        assert_eq!(state_path(d, Some("..\\..")), d.join("tubes").join("default.state"));
    }

    /// A fake renderer: the map lives in memory, snapshots arrive one tick after the request.
    #[derive(Default)]
    struct Fake {
        map: Option<WearMap>,
        requested: bool,
        ready: bool,
        resets: u32,
        reads: u32,
        burned: f32,
    }

    impl WearHost for Fake {
        fn wear_map_size(&self) -> (u32, u32) {
            (4, 3)
        }
        fn set_wear_map(&mut self, map: &WearMap) -> Res<()> {
            self.map = Some(map.clone());
            Ok(())
        }
        fn reset_wear(&mut self) {
            self.map = Some(WearMap::new(4, 3));
            self.resets += 1;
        }
        fn burn_in_now(&mut self, hours: f32) {
            self.burned += hours;
        }
        fn request_wear_snapshot(&mut self) {
            self.requested = true;
        }
        fn take_wear_snapshot(&mut self) -> Res<Option<WearMap>> {
            if self.ready {
                self.ready = false;
                return Ok(self.map.clone());
            }
            self.ready = std::mem::take(&mut self.requested);
            Ok(None)
        }
        fn read_wear_map(&mut self) -> Res<Option<WearMap>> {
            self.reads += 1;
            Ok(self.map.clone())
        }
    }

    /// The controller: nothing while burn-in is off; loads at the first frame with it on, saves periodically
    /// without blocking, swaps maps with the preset, saves at exit; --reset-burnin starts a new tube.
    #[test]
    fn controller_lifecycle() {
        let dir = temp_dir("ctl");
        let t0 = Instant::now();
        let mut ts = TubeState::new(Some(dir.clone()), false, t0);
        let mut host = Fake::default();
        let mut p = Params::default();
        ts.tick(&mut host, &p, Some("a"), t0);
        assert!(host.map.is_none() && ts.loaded_path().is_none(), "burn-in off: nothing");
        p.burnin_enable = true;
        ts.tick(&mut host, &p, Some("a"), t0);
        assert_eq!(host.resets, 1, "missing file: new tube");
        assert_eq!(ts.loaded_path(), Some(state_path(&dir, Some("a")).as_path()));
        // Wear appears; the periodic save is asynchronous.
        host.map = Some(sample(4, 3));
        ts.tick(&mut host, &p, Some("a"), t0 + SAVE_PERIOD);
        assert!(host.requested);
        ts.tick(&mut host, &p, Some("a"), t0 + SAVE_PERIOD);
        ts.tick(&mut host, &p, Some("a"), t0 + SAVE_PERIOD);
        ts.join_writer();
        assert_eq!(host.reads, 0, "no blocking read for the periodic save");
        let saved = load(&state_path(&dir, Some("a")), (4, 3));
        assert_eq!(
            saved,
            Loaded::Map {
                map: sample(4, 3),
                resampled_from: None
            }
        );
        // Another preset: the old map is read back and saved, the new one loaded (missing: new tube).
        host.map = Some(WearMap {
            q: vec![5.0; 36],
            ..WearMap::new(4, 3)
        });
        ts.tick(&mut host, &p, Some("b"), t0 + SAVE_PERIOD);
        assert_eq!(host.reads, 1);
        assert_eq!(host.resets, 2);
        match load(&state_path(&dir, Some("a")), (4, 3)) {
            Loaded::Map { map, .. } => assert_eq!(map.q, vec![5.0; 36]),
            other => panic!("{other:?}"),
        }
        // Back to a: loaded from its file.
        ts.tick(&mut host, &p, Some("a"), t0 + SAVE_PERIOD);
        assert_eq!(host.map.as_ref().unwrap().q, vec![5.0; 36]);
        // Burn in now and reset.
        ts.burn_in_now(&mut host, 100.0);
        assert_eq!(host.burned, 100.0);
        ts.reset(&mut host);
        assert_eq!(host.map, Some(WearMap::new(4, 3)));
        match load(&state_path(&dir, Some("a")), (4, 3)) {
            Loaded::Map { map, .. } => assert_eq!(map.max_charge(), 0.0),
            other => panic!("{other:?}"),
        }
        // Exit: read back and written.
        host.map = Some(sample(4, 3));
        ts.finish(&mut host);
        assert!(matches!(load(&state_path(&dir, Some("a")), (4, 3)), Loaded::Map { map, .. } if map == sample(4, 3)));
        // --reset-burnin: the stored wear is not loaded.
        let mut ts = TubeState::new(Some(dir.clone()), true, t0);
        let mut host = Fake::default();
        ts.tick(&mut host, &p, Some("a"), t0);
        assert_eq!(host.map, Some(WearMap::new(4, 3)));
        assert!(ts.take_messages().iter().any(|m| m.contains("--reset-burnin")));
        let _ = fs::remove_dir_all(&dir);
    }

    /// A renderer whose snapshot is a copy of the map when it was asked for (as on the GPU), delivered one tick
    /// late.
    #[derive(Default)]
    struct Late {
        map: Option<WearMap>,
        in_flight: Option<(WearMap, u32)>,
    }

    impl WearHost for Late {
        fn wear_map_size(&self) -> (u32, u32) {
            (4, 3)
        }
        fn set_wear_map(&mut self, map: &WearMap) -> Res<()> {
            self.map = Some(map.clone());
            Ok(())
        }
        fn reset_wear(&mut self) {
            self.map = Some(WearMap::new(4, 3));
        }
        fn burn_in_now(&mut self, _: f32) {}
        fn request_wear_snapshot(&mut self) {
            self.in_flight = self.map.clone().map(|m| (m, 0));
        }
        fn take_wear_snapshot(&mut self) -> Res<Option<WearMap>> {
            match self.in_flight.take() {
                Some((m, age)) if age >= 1 => Ok(Some(m)),
                Some((m, age)) => {
                    self.in_flight = Some((m, age + 1));
                    Ok(None)
                }
                None => Ok(None),
            }
        }
        fn read_wear_map(&mut self) -> Res<Option<WearMap>> {
            Ok(self.map.clone())
        }
    }

    fn uniform(v: f32) -> WearMap {
        WearMap {
            q: vec![v; 36],
            ..WearMap::new(4, 3)
        }
    }

    fn stored(path: &Path) -> Option<f32> {
        match load(path, (4, 3)) {
            Loaded::Map { map, .. } => Some(map.max_charge()),
            _ => None,
        }
    }

    /// R-06: a snapshot asked for in preset A (tick N) that arrives after the switch to B (tick N + 2) is
    /// dropped: B's file keeps B's map, A's file has A's map from the switch.
    #[test]
    fn a_snapshot_from_before_a_preset_switch_is_dropped() {
        let dir = temp_dir("late-switch");
        let (a, b) = (state_path(&dir, Some("a")), state_path(&dir, Some("b")));
        save(&b, &uniform(2.0)).unwrap();
        let t0 = Instant::now();
        let mut ts = TubeState::new(Some(dir.clone()), false, t0);
        let mut host = Late::default();
        let p = Params {
            burnin_enable: true,
            ..Params::default()
        };
        ts.tick(&mut host, &p, Some("a"), t0);
        host.map = Some(uniform(5.0));
        let n = t0 + SAVE_PERIOD;
        ts.tick(&mut host, &p, Some("a"), n); // N: the snapshot of A's map is asked for
        assert!(host.in_flight.is_some());
        ts.tick(&mut host, &p, Some("b"), n); // N + 1: switch, B's map loaded from its file
        assert_eq!(host.map, Some(uniform(2.0)));
        assert_eq!(stored(&a), Some(5.0), "A saved at the switch");
        ts.tick(&mut host, &p, Some("b"), n); // N + 2: A's snapshot arrives
        assert!(host.in_flight.is_none(), "delivered");
        ts.join_writer();
        assert_eq!(stored(&b), Some(2.0), "B's file still holds B's map");
        // The next periodic save of B works as before.
        host.map = Some(uniform(3.0));
        let m = n + SAVE_PERIOD;
        for _ in 0..3 {
            ts.tick(&mut host, &p, Some("b"), m);
        }
        ts.join_writer();
        assert_eq!(stored(&b), Some(3.0));
        let _ = fs::remove_dir_all(&dir);
    }

    /// R-06: a snapshot asked for before "New tube" does not undo it: the file stays a new tube.
    #[test]
    fn a_snapshot_from_before_a_new_tube_is_dropped() {
        let dir = temp_dir("late-reset");
        let a = state_path(&dir, Some("a"));
        let t0 = Instant::now();
        let mut ts = TubeState::new(Some(dir.clone()), false, t0);
        let mut host = Late::default();
        let p = Params {
            burnin_enable: true,
            ..Params::default()
        };
        ts.tick(&mut host, &p, Some("a"), t0);
        host.map = Some(uniform(5.0));
        let n = t0 + SAVE_PERIOD;
        ts.tick(&mut host, &p, Some("a"), n); // N: snapshot of the worn map asked for
        ts.tick(&mut host, &p, Some("a"), n); // N + 1: still in flight
        ts.reset(&mut host); // "New tube"
        assert_eq!(stored(&a), Some(0.0));
        ts.tick(&mut host, &p, Some("a"), n); // N + 2: the worn snapshot arrives
        assert!(host.in_flight.is_none());
        ts.join_writer();
        assert_eq!(stored(&a), Some(0.0), "the new tube stays new");
        let _ = fs::remove_dir_all(&dir);
    }

    /// R-54: without a settings folder the controller reads and writes no file (a key is kept, not a path).
    #[test]
    fn no_settings_folder_means_no_file_access() {
        let t0 = Instant::now();
        let mut ts = TubeState::new(None, false, t0);
        let mut host = Fake {
            map: Some(uniform(4.0)),
            ..Fake::default()
        };
        let p = Params {
            burnin_enable: true,
            ..Params::default()
        };
        ts.tick(&mut host, &p, Some("a"), t0);
        assert_eq!((host.resets, host.map.clone()), (1, Some(WearMap::new(4, 3))));
        assert!(ts.take_messages().iter().any(|m| m.contains("no settings folder")));
        assert!(ts.loaded_path().is_some_and(Path::is_relative));
        host.map = Some(uniform(4.0));
        ts.tick(&mut host, &p, Some("b"), t0); // a switch: nothing written for a, nothing read for b
        ts.reset(&mut host);
        ts.finish(&mut host);
        assert!(ts.take_messages().iter().all(|m| !m.contains("warning")));
    }
}
