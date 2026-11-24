//! A video capture device (a capture card, a webcam: any UVC device Windows lists) as the source, through Media
//! Foundation's source reader (TODO Low, "capture card").
//!
//! The source reader is asked for RGB32 with its video processing on, so Windows converts whatever the device sends
//! (YUY2, NV12, MJPEG, ...) to BGRX; this module copies each sample to the tightly packed top-down BGRA8 [`Frame`]
//! the rest of the app already takes from a window capture, an image or a pattern ([`bgrx_to_bgra`]). The device
//! delivers its own native raster, so the source is native (divisor 1), as for an image.
//!
//! One worker thread per open device (COM and Media Foundation started on it) reads samples synchronously and keeps
//! the newest; [`VideoCapture::take_frame`] hands it over as [`crate::capture::Frame`] does for a capture. Listing
//! devices ([`list`]) runs on a short-lived thread of its own for the same reason. Nothing here is a capture of a
//! window: the output window's own pixels are never involved.
//!
//! Limits: the device's default mode is used (no mode choice); interlaced or limited-range signals are shown as
//! Windows converts them; no audio. Built against the Media Foundation API and checked on the development
//! laptop's own camera only (see `docs/REFERENCE.md`); not checked with a real capture card.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::capture::Frame;

/// A video capture device as the picker lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    /// The name Windows shows for it (`MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME`).
    pub name: String,
    /// Its symbolic link, the stable way to find it again (`..._VIDCAP_SYMBOLIC_LINK`).
    pub link: String,
}

/// The frame size packed in a Media Foundation `UINT64` attribute (`MF_MT_FRAME_SIZE`): width in the high half,
/// height in the low half.
#[must_use]
pub fn unpack_size(v: u64) -> (u32, u32) {
    ((v >> 32) as u32, (v & 0xFFFF_FFFF) as u32)
}

/// Copies an RGB32 sample (B, G, R, then an unused byte per pixel) to tightly packed top-down BGRA8 with alpha 255.
/// `stride` is the sample's row pitch in bytes as Media Foundation reports it (`MF_MT_DEFAULT_STRIDE`): positive for
/// a top-down sample, negative for a bottom-up one (the first row in memory is the bottom row of the picture).
/// `None` when the sizes are empty or the buffer is shorter than `height` rows of `|stride|` bytes.
#[must_use]
pub fn bgrx_to_bgra(src: &[u8], width: u32, height: u32, stride: i32) -> Option<Vec<u8>> {
    let (w, h) = (width as usize, height as usize);
    let pitch = stride.unsigned_abs() as usize;
    if w == 0 || h == 0 || pitch < w * 4 || src.len() < pitch * h {
        return None;
    }
    let mut out = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        let row = if stride < 0 { h - 1 - y } else { y };
        let line = &src[row * pitch..row * pitch + w * 4];
        for px in line.as_chunks::<4>().0 {
            out.extend_from_slice(&[px[0], px[1], px[2], 255]);
        }
    }
    Some(out)
}

/// The row pitch of a sample, signed as [`bgrx_to_bgra`] takes it. A device that reports `MF_MT_DEFAULT_STRIDE` (`attr`)
/// is believed. Without the attribute Media Foundation's convention for uncompressed RGB applies: the rows are
/// bottom-up (a negative stride), and the pitch is read from the sample itself when its length is a whole number of
/// rows at least `width` pixels wide (a converter that pads its rows), else it is `width * 4`.
#[must_use]
pub fn effective_stride(attr: Option<i32>, width: u32, height: u32, len: usize) -> i32 {
    if let Some(s) = attr {
        return s;
    }
    let tight = (width as usize) * 4;
    let rows = height as usize;
    let pitch = if rows > 0 && len.is_multiple_of(rows) && len / rows >= tight {
        len / rows
    } else {
        tight
    };
    -(i32::try_from(pitch).unwrap_or(i32::MAX))
}

/// A frame as the worker keeps it: its sequence number (from 1), width, height and BGRA8 pixels.
type Sample = (u64, u32, u32, Vec<u8>);

/// Where a video device is in its opening ([`VideoCapture::status`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Status {
    /// The worker is opening the device (it can take seconds: a card that wakes up slowly).
    #[default]
    Opening,
    /// The device is open and delivers frames of this size.
    Running((u32, u32)),
    /// The device could not be used: the reason.
    Failed(String),
}

/// How long dropping a capture waits for its worker before leaving it behind (see `Drop for VideoCapture`).
const STOP_WAIT: Duration = Duration::from_millis(1500);

/// The newest frame of the worker, and the flags it shares with the owner.
#[derive(Default)]
struct Shared {
    frame: Mutex<Option<Sample>>,
    status: Mutex<Status>,
    stop: AtomicBool,
    /// The worker ended (the device was unplugged, or stopped delivering).
    ended: AtomicBool,
    /// Shuts the media source down: a worker blocked in a read (a card that sends nothing) returns with an error
    /// as soon as it is called, from any thread. Set by the worker once the device is open.
    shutdown: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl Shared {
    /// The frame slot, whoever panicked while holding it: a poisoned lock holds a whole `Option` (the worker stores it
    /// in one assignment), so its content is as good as any, and the device keeps working instead of delivering
    /// nothing for ever.
    fn lock_frame(&self) -> std::sync::MutexGuard<'_, Option<Sample>> {
        self.frame.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn set_status(&self, status: Status) {
        *self.status.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = status;
    }

    fn status(&self) -> Status {
        self.status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Calls the shutdown of the media source, once, if the worker has published one.
    fn shut_source_down(&self) {
        let f = self
            .shutdown
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(f) = f {
            f();
        }
    }
}

/// An open video capture device (see the module documentation). Dropping it stops the worker.
pub struct VideoCapture {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
    /// The device's name, for the title.
    pub name: String,
}

impl VideoCapture {
    /// Starts opening `device` on a worker thread and returns at once: the render thread never waits for a device
    /// (a card can take seconds to wake up, or never answer). Poll [`VideoCapture::status`] and
    /// [`VideoCapture::take_frame`]; an error is the reason the device cannot be used.
    pub fn start(device: &Device) -> Result<VideoCapture, String> {
        let shared = Arc::new(Shared::default());
        let link = device.link.clone();
        let worker_shared = Arc::clone(&shared);
        let worker = std::thread::Builder::new()
            .name("phoxium-video".into())
            .spawn(move || platform::run(&link, &worker_shared))
            .map_err(|e| e.to_string())?;
        Ok(VideoCapture {
            shared,
            worker: Some(worker),
            name: device.name.clone(),
        })
    }

    /// [`VideoCapture::start`], waiting for the device's mode (5 s at most; the tests and tools). An error is the
    /// reason the device cannot be used.
    pub fn open(device: &Device) -> Result<VideoCapture, String> {
        let cap = VideoCapture::start(device)?;
        let t0 = Instant::now();
        loop {
            match cap.status() {
                Status::Running(_) => return Ok(cap),
                Status::Failed(e) => return Err(e),
                Status::Opening if t0.elapsed() > Duration::from_secs(5) => {
                    return Err("the device did not answer within 5 seconds".into())
                }
                Status::Opening => std::thread::sleep(Duration::from_millis(10)),
            }
        }
    }

    /// Where the opening is: still opening, running at a frame size, or failed with a reason.
    pub fn status(&self) -> Status {
        self.shared.status()
    }

    /// The frame size of the device's mode once it is open.
    pub fn size(&self) -> Option<(u32, u32)> {
        match self.shared.status() {
            Status::Running(size) => Some(size),
            _ => None,
        }
    }

    /// Hands over the newest frame if it is newer than `into.seq`: true when `into` now holds a new frame. The
    /// frames count from 1 in this session; `into.session` is 0 (a CPU frame).
    pub fn take_frame(&self, into: &mut Frame) -> bool {
        let mut f = self.shared.lock_frame();
        match f.take() {
            Some((seq, w, h, data)) if seq != into.seq => {
                *into = Frame::cpu(w, h, data);
                into.seq = seq;
                true
            }
            other => {
                *f = other;
                false
            }
        }
    }

    /// Whether the device stopped delivering (unplugged, or the stream ended).
    pub fn ended(&self) -> bool {
        self.shared.ended.load(Ordering::Relaxed)
    }
}

impl Drop for VideoCapture {
    /// Stops the worker without ever hanging the caller. A worker blocked in a read (a card that sends no picture) is
    /// released by shutting its media source down from here; if it still has not finished after [`STOP_WAIT`] it is
    /// left behind (it ends by itself when its read returns, and closes the device then) instead of freezing the app.
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.shared.shut_source_down();
        let Some(w) = self.worker.take() else {
            return;
        };
        let t0 = Instant::now();
        while !w.is_finished() && t0.elapsed() < STOP_WAIT {
            std::thread::sleep(Duration::from_millis(5));
        }
        if w.is_finished() {
            let _ = w.join();
        } else {
            println!("video: the device did not stop within {STOP_WAIT:?}; left to finish in the background");
        }
    }
}

/// The video capture devices Windows lists now (empty on any failure: no device, Media Foundation missing).
#[must_use]
pub fn list() -> Vec<Device> {
    std::thread::Builder::new()
        .name("phoxium-video-list".into())
        .spawn(platform::list)
        .ok()
        .and_then(|h| h.join().ok())
        .unwrap_or_default()
}

mod platform {
    use super::{bgrx_to_bgra, effective_stride, unpack_size, Device, Shared, Status};
    use std::sync::atomic::Ordering;
    use windows::core::{Error, PWSTR};
    use windows::Win32::Foundation::E_FAIL;
    use windows::Win32::Media::MediaFoundation::{
        IMFActivate, IMFAttributes, IMFMediaSource, IMFSourceReader, MFCreateAttributes, MFCreateMediaType,
        MFCreateSourceReaderFromMediaSource, MFEnumDeviceSources, MFMediaType_Video, MFShutdown, MFStartup,
        MFVideoFormat_RGB32, MFSTARTUP_FULL, MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
        MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK,
        MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE,
        MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED, MF_SOURCE_READERF_ENDOFSTREAM, MF_SOURCE_READERF_ERROR,
        MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_VERSION,
    };
    use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_MULTITHREADED};

    /// Media Foundation and COM started on this thread, stopped when dropped.
    struct Started;

    impl Started {
        fn new() -> Result<Started, Error> {
            // SAFETY: plain start-up calls on this thread; every success is paired with a shutdown in `Drop`. A COM
            // apartment already set to another mode (S_FALSE, RPC_E_CHANGED_MODE) is not an error here: Media
            // Foundation works in either.
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                MFStartup(MF_VERSION, MFSTARTUP_FULL)?;
            }
            Ok(Started)
        }
    }

    impl Drop for Started {
        fn drop(&mut self) {
            // SAFETY: pairs the calls of `new`.
            unsafe {
                let _ = MFShutdown();
                CoUninitialize();
            }
        }
    }

    /// The activation objects of the video capture devices.
    unsafe fn enumerate() -> Result<Vec<IMFActivate>, Error> {
        let mut attrs: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attrs, 1)?;
        let attrs = attrs.ok_or_else(|| Error::from(E_FAIL))?;
        attrs.SetGUID(
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
        )?;
        let mut array: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count = 0u32;
        MFEnumDeviceSources(&attrs, &mut array, &mut count)?;
        let mut out = Vec::new();
        if !array.is_null() {
            for i in 0..count as usize {
                if let Some(a) = (*array.add(i)).take() {
                    out.push(a);
                }
            }
            CoTaskMemFree(Some(array as *const _));
        }
        Ok(out)
    }

    /// A string attribute of `activate` (empty when absent).
    unsafe fn string_of(activate: &IMFActivate, key: &windows::core::GUID) -> String {
        let mut p = PWSTR::null();
        let mut len = 0u32;
        if activate.GetAllocatedString(key, &mut p, &mut len).is_err() || p.is_null() {
            return String::new();
        }
        let s = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.0 as *const _));
        s
    }

    fn device_of(a: &IMFActivate) -> Device {
        // SAFETY: `a` is a live activation object; the strings are copied and freed at once.
        unsafe {
            Device {
                name: string_of(a, &MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME),
                link: string_of(a, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK),
            }
        }
    }

    pub fn list() -> Vec<Device> {
        let Ok(_mf) = Started::new() else {
            return Vec::new();
        };
        // SAFETY: Media Foundation is started on this thread.
        let found = unsafe { enumerate() }.unwrap_or_default();
        found
            .iter()
            .map(device_of)
            .filter(|d| !d.link.is_empty() || !d.name.is_empty())
            .collect()
    }

    /// The source reader of the device with the symbolic link `link`, asked for RGB32.
    unsafe fn open_reader(link: &str) -> Result<(IMFMediaSource, IMFSourceReader), String> {
        let activate = enumerate()
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|a| device_of(a).link == link)
            .ok_or("the device is not there any more")?;
        let source: IMFMediaSource = activate
            .ActivateObject()
            .map_err(|e| format!("cannot open the device: {e}"))?;
        let mut attrs: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attrs, 1).map_err(|e| e.to_string())?;
        let attrs = attrs.ok_or("no attributes")?;
        // Video processing lets the reader convert the device's format (YUY2, NV12, MJPEG, ...) to RGB32.
        attrs
            .SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)
            .map_err(|e| e.to_string())?;
        let reader =
            MFCreateSourceReaderFromMediaSource(&source, &attrs).map_err(|e| format!("cannot read the device: {e}"))?;
        let wanted = MFCreateMediaType().map_err(|e| e.to_string())?;
        wanted
            .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(|e| e.to_string())?;
        wanted
            .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)
            .map_err(|e| e.to_string())?;
        reader
            .SetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, None, &wanted)
            .map_err(|e| format!("the device cannot give RGB: {e}"))?;
        Ok((source, reader))
    }

    /// The size and the row pitch (`None` when the type has no `MF_MT_DEFAULT_STRIDE`, see [`effective_stride`]) of
    /// the reader's current RGB32 type.
    unsafe fn mode_of(reader: &IMFSourceReader) -> Result<((u32, u32), Option<i32>), String> {
        let t = reader
            .GetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32)
            .map_err(|e| e.to_string())?;
        let (w, h) = unpack_size(t.GetUINT64(&MF_MT_FRAME_SIZE).map_err(|e| e.to_string())?);
        let stride = t.GetUINT32(&MF_MT_DEFAULT_STRIDE).ok().map(|v| v as i32);
        Ok(((w, h), stride))
    }

    /// A media source that another thread may shut down: media sources are free-threaded (the worker lives in a
    /// multithreaded COM apartment) and `Shutdown` is the documented way to stop one from outside.
    struct Agile(IMFMediaSource);

    // SAFETY: see the type's documentation.
    unsafe impl Send for Agile {}

    impl Agile {
        fn shutdown(&self) {
            // SAFETY: a live media source; a second shutdown only returns an error.
            unsafe {
                let _ = self.0.Shutdown();
            }
        }
    }

    /// The worker: opens the device, publishes its mode (or the error) in `shared.status`, then reads samples until
    /// asked to stop or the stream ends.
    pub fn run(link: &str, shared: &Shared) {
        // Whatever ends the worker (the stream, an error, a panic), the owner sees `ended`.
        struct EndGuard<'a>(&'a Shared);
        impl Drop for EndGuard<'_> {
            fn drop(&mut self) {
                self.0.ended.store(true, Ordering::Relaxed);
            }
        }
        let _end = EndGuard(shared);
        let started = Started::new();
        let Ok(_mf) = started else {
            shared.set_status(Status::Failed("Media Foundation is not available".into()));
            return;
        };
        // SAFETY: Media Foundation is started on this thread; every COM object below lives and dies here.
        unsafe {
            let (source, reader) = match open_reader(link) {
                Ok(v) => v,
                Err(e) => {
                    shared.set_status(Status::Failed(e));
                    return;
                }
            };
            let (mut size, mut stride) = match mode_of(&reader) {
                Ok(v) => v,
                Err(e) => {
                    shared.set_status(Status::Failed(e));
                    let _ = source.Shutdown();
                    return;
                }
            };
            // The owner can shut the source down from outside to release a blocked read.
            let agile = Agile(source.clone());
            *shared
                .shutdown
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Box::new(move || agile.shutdown()));
            shared.set_status(Status::Running(size));
            let mut seq = 0u64;
            while !shared.stop.load(Ordering::Relaxed) {
                let mut flags = 0u32;
                let mut sample = None;
                let read = reader.ReadSample(
                    MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                    0,
                    None,
                    Some(&mut flags),
                    None,
                    Some(&mut sample),
                );
                if read.is_err()
                    || flags & (MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 | MF_SOURCE_READERF_ERROR.0 as u32) != 0
                {
                    // Stopped from outside (the source was shut down to release this read), or the device ended.
                    shared.ended.store(true, Ordering::Relaxed);
                    break;
                }
                if flags & MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0 as u32 != 0 {
                    if let Ok(m) = mode_of(&reader) {
                        (size, stride) = m;
                    }
                }
                let Some(sample) = sample else {
                    continue;
                };
                let Ok(buffer) = sample.ConvertToContiguousBuffer() else {
                    continue;
                };
                let (mut ptr, mut len) = (std::ptr::null_mut::<u8>(), 0u32);
                if buffer.Lock(&mut ptr, None, Some(&mut len)).is_err() || ptr.is_null() {
                    continue;
                }
                let bytes = std::slice::from_raw_parts(ptr, len as usize);
                let converted = bgrx_to_bgra(
                    bytes,
                    size.0,
                    size.1,
                    effective_stride(stride, size.0, size.1, bytes.len()),
                );
                let _ = buffer.Unlock();
                if let Some(data) = converted {
                    seq += 1;
                    *shared.lock_frame() = Some((seq, size.0, size.1, data));
                }
            }
            drop(reader);
            let _ = source.Shutdown();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The attribute wins; without it the rows are bottom-up (Media Foundation's RGB convention) with the pitch of the
    /// sample when it holds whole padded rows, else the tight one.
    #[test]
    fn a_missing_stride_means_bottom_up_with_the_pitch_of_the_sample() {
        assert_eq!(effective_stride(Some(12), 2, 2, 24), 12);
        assert_eq!(effective_stride(Some(-8), 2, 2, 16), -8);
        assert_eq!(effective_stride(None, 2, 2, 16), -8, "tight rows");
        assert_eq!(effective_stride(None, 2, 2, 24), -12, "padded rows");
        assert_eq!(
            effective_stride(None, 2, 3, 25),
            -8,
            "not a whole number of rows: tight"
        );
        assert_eq!(
            effective_stride(None, 4, 2, 16),
            -16,
            "a buffer shorter than the rows needs: tight, refused later"
        );
        assert_eq!(effective_stride(None, 2, 0, 16), -8, "no rows");
    }

    /// With no attribute a bottom-up sample comes out the right way up.
    #[test]
    fn a_sample_without_the_attribute_is_turned_over() {
        let src = [
            1, 1, 1, 0, 2, 2, 2, 0, // memory row 0 = the bottom row
            3, 3, 3, 0, 4, 4, 4, 0, // memory row 1 = the top row
        ];
        let stride = effective_stride(None, 2, 2, src.len());
        let out = bgrx_to_bgra(&src, 2, 2, stride).unwrap();
        assert_eq!(&out[..4], &[3, 3, 3, 255], "the first output pixel is the top-left one");
        assert_eq!(&out[12..], &[2, 2, 2, 255]);
    }

    #[test]
    fn the_frame_size_is_unpacked_from_the_attribute() {
        assert_eq!(unpack_size((1920u64 << 32) | 1080), (1920, 1080));
        assert_eq!(unpack_size(0), (0, 0));
    }

    /// A 2 x 2 top-down RGB32 sample with a 12-byte row pitch (4 bytes of padding per row): the pixels keep their
    /// order, the unused byte becomes alpha 255, the padding is dropped.
    #[test]
    fn a_top_down_sample_is_repacked_with_alpha() {
        let src = [
            1, 2, 3, 0, 4, 5, 6, 0, 9, 9, 9, 9, // row 0 and its padding
            7, 8, 9, 7, 10, 11, 12, 7, 9, 9, 9, 9, // row 1
        ];
        let out = bgrx_to_bgra(&src, 2, 2, 12).unwrap();
        assert_eq!(out, [1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 255]);
    }

    /// A bottom-up sample (negative stride) has its rows turned over: the last row in memory is the top row.
    #[test]
    fn a_bottom_up_sample_is_turned_over() {
        let src = [
            1, 1, 1, 0, 2, 2, 2, 0, // memory row 0 = the bottom row
            3, 3, 3, 0, 4, 4, 4, 0, // memory row 1 = the top row
        ];
        let out = bgrx_to_bgra(&src, 2, 2, -8).unwrap();
        assert_eq!(out, [3, 3, 3, 255, 4, 4, 4, 255, 1, 1, 1, 255, 2, 2, 2, 255]);
    }

    /// An empty size, a pitch narrower than a row and a short buffer give nothing instead of reading past the end.
    #[test]
    fn a_bad_sample_gives_nothing() {
        assert!(bgrx_to_bgra(&[0; 16], 0, 2, 8).is_none());
        assert!(bgrx_to_bgra(&[0; 16], 2, 0, 8).is_none());
        assert!(bgrx_to_bgra(&[0; 16], 2, 2, 4).is_none(), "pitch narrower than a row");
        assert!(bgrx_to_bgra(&[0; 15], 2, 2, 8).is_none(), "buffer too short");
        assert!(bgrx_to_bgra(&[0; 16], 2, 2, 8).is_some());
    }

    /// The owner releases a worker that is blocked in a read: the shutdown the worker published is called (once), the
    /// drop returns as soon as the worker has finished, and a worker that never finishes does not hang the drop (it is
    /// left behind after the wait). Both with a fake worker thread, no device.
    #[test]
    fn dropping_a_capture_never_hangs_on_a_blocked_worker() {
        use std::sync::atomic::AtomicUsize;
        // A worker that is "blocked in a read" until the shutdown is called: it then finishes.
        let shared = Arc::new(Shared::default());
        let shutdowns = Arc::new(AtomicUsize::new(0));
        let released = Arc::new(AtomicBool::new(false));
        {
            let (count, flag) = (Arc::clone(&shutdowns), Arc::clone(&released));
            *shared.shutdown.lock().unwrap() = Some(Box::new(move || {
                count.fetch_add(1, Ordering::Relaxed);
                flag.store(true, Ordering::Relaxed);
            }));
        }
        let flag = Arc::clone(&released);
        let worker = std::thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(2));
            }
        });
        let cap = VideoCapture {
            shared: Arc::clone(&shared),
            worker: Some(worker),
            name: "blocked".into(),
        };
        let t0 = Instant::now();
        drop(cap);
        assert!(t0.elapsed() < Duration::from_millis(800), "{:?}", t0.elapsed());
        assert_eq!(shutdowns.load(Ordering::Relaxed), 1, "the source was shut down once");
        assert!(shared.stop.load(Ordering::Relaxed));

        // A worker that ignores everything: the drop gives up after the wait instead of joining for ever.
        let shared = Arc::new(Shared::default());
        let gate = Arc::new(AtomicBool::new(false));
        let g = Arc::clone(&gate);
        let stuck = std::thread::spawn(move || {
            while !g.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        let cap = VideoCapture {
            shared,
            worker: Some(stuck),
            name: "stuck".into(),
        };
        let t0 = Instant::now();
        drop(cap);
        let waited = t0.elapsed();
        assert!(
            waited >= STOP_WAIT && waited < STOP_WAIT + Duration::from_millis(1500),
            "{waited:?}"
        );
        gate.store(true, Ordering::Relaxed); // let the left-behind thread end
    }

    /// The status moves from opening to running or failed, and `size` follows it.
    #[test]
    fn the_status_follows_the_opening() {
        let shared = Arc::new(Shared::default());
        let cap = VideoCapture {
            shared: Arc::clone(&shared),
            worker: None,
            name: "t".into(),
        };
        assert_eq!((cap.status(), cap.size()), (Status::Opening, None));
        shared.set_status(Status::Running((640, 480)));
        assert_eq!(
            (cap.status(), cap.size()),
            (Status::Running((640, 480)), Some((640, 480)))
        );
        shared.set_status(Status::Failed("busy".into()));
        assert_eq!((cap.status(), cap.size()), (Status::Failed("busy".into()), None));
    }

    /// A panic while the frame slot is locked (the worker, or anything else) does not stop the frames for good: the
    /// poisoned lock is read through, and the worker's end is always reported.
    #[test]
    fn a_poisoned_lock_does_not_stop_the_frames() {
        let shared = Arc::new(Shared::default());
        let held = Arc::clone(&shared);
        let _ = std::thread::spawn(move || {
            let _guard = held.frame.lock().unwrap();
            panic!("a worker that panics while it holds the frame slot");
        })
        .join();
        assert!(shared.frame.lock().is_err(), "the lock is poisoned");
        let cap = VideoCapture {
            shared: Arc::clone(&shared),
            worker: None,
            name: "test".into(),
        };
        *shared.lock_frame() = Some((1, 1, 1, vec![9; 4]));
        let mut into = Frame::default();
        assert!(
            cap.take_frame(&mut into),
            "the frame is delivered through the poisoned lock"
        );
        assert_eq!(into.data, [9; 4]);
    }

    /// The newest frame is handed over once; the same sequence number again is not new; a newer one is.
    #[test]
    fn a_frame_is_taken_once() {
        let shared = Arc::new(Shared::default());
        let cap = VideoCapture {
            shared: Arc::clone(&shared),
            worker: None,
            name: "test".into(),
        };
        let mut into = Frame::default();
        assert!(!cap.take_frame(&mut into), "nothing yet");
        *shared.frame.lock().unwrap() = Some((1, 2, 1, vec![0; 8]));
        assert!(cap.take_frame(&mut into));
        assert_eq!((into.width, into.height, into.seq, into.data.len()), (2, 1, 1, 8));
        assert!(!cap.take_frame(&mut into), "taken");
        *shared.frame.lock().unwrap() = Some((1, 2, 1, vec![0; 8]));
        assert!(!cap.take_frame(&mut into), "the same sequence number is not new");
        *shared.frame.lock().unwrap() = Some((2, 2, 1, vec![1; 8]));
        assert!(cap.take_frame(&mut into));
        assert_eq!((into.seq, into.data[0]), (2, 1));
        assert!(!cap.ended());
        shared.ended.store(true, Ordering::Relaxed);
        assert!(cap.ended());
    }

    /// Hardware check, only with `PHOXIUM_VIDEO_TEST=1` (it opens the first video device, a camera lights up): the
    /// device lists, opens, and delivers a frame of its mode's size within five seconds. Only sizes are read, no
    /// picture is kept.
    #[test]
    fn the_first_video_device_delivers_a_frame() {
        if std::env::var("PHOXIUM_VIDEO_TEST").as_deref() != Ok("1") {
            eprintln!("video device test skipped (set PHOXIUM_VIDEO_TEST=1)");
            return;
        }
        let devices = list();
        eprintln!(
            "video devices: {:?}",
            devices.iter().map(|d| d.name.as_str()).collect::<Vec<_>>()
        );
        let Some(device) = devices.first() else {
            eprintln!("no video device here");
            return;
        };
        let cap = VideoCapture::open(device).expect("open");
        assert!(matches!(cap.status(), Status::Running(_)));
        let mut frame = Frame::default();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !cap.take_frame(&mut frame) {
            assert!(std::time::Instant::now() < deadline, "no frame within 5 seconds");
            std::thread::sleep(Duration::from_millis(10));
        }
        eprintln!("frame {}x{}, mode {:?}", frame.width, frame.height, cap.size());
        assert_eq!(Some((frame.width, frame.height)), cap.size());
        assert_eq!(frame.data.len(), (frame.width * frame.height * 4) as usize);
    }
}
