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
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

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

/// A frame as the worker keeps it: its sequence number (from 1), width, height and BGRA8 pixels.
type Sample = (u64, u32, u32, Vec<u8>);

/// The newest frame of the worker, and the flags it shares with the owner.
#[derive(Default)]
struct Shared {
    frame: Mutex<Option<Sample>>,
    stop: AtomicBool,
    /// The worker ended (the device was unplugged, or stopped delivering).
    ended: AtomicBool,
}

impl Shared {
    /// The frame slot, whoever panicked while holding it: a poisoned lock holds a whole `Option` (the worker stores it
    /// in one assignment), so its content is as good as any, and the device keeps working instead of delivering
    /// nothing for ever.
    fn lock_frame(&self) -> std::sync::MutexGuard<'_, Option<Sample>> {
        self.frame.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// An open video capture device (see the module documentation). Dropping it stops the worker.
pub struct VideoCapture {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
    /// The device's name, for the title.
    pub name: String,
    /// The frame size of the device's current mode.
    pub size: (u32, u32),
}

impl VideoCapture {
    /// Opens `device` and starts reading. Waits for the worker to report the device's mode (5 s at most); an error
    /// is the reason the device cannot be used.
    pub fn open(device: &Device) -> Result<VideoCapture, String> {
        let shared = Arc::new(Shared::default());
        let (tx, rx) = mpsc::channel::<Result<(u32, u32), String>>();
        let link = device.link.clone();
        let worker_shared = Arc::clone(&shared);
        let worker = std::thread::Builder::new()
            .name("phoxium-video".into())
            .spawn(move || platform::run(&link, &worker_shared, &tx))
            .map_err(|e| e.to_string())?;
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(size)) => Ok(VideoCapture {
                shared,
                worker: Some(worker),
                name: device.name.clone(),
                size,
            }),
            Ok(Err(e)) => {
                let _ = worker.join();
                Err(e)
            }
            Err(_) => {
                shared.stop.store(true, Ordering::Relaxed);
                Err("the device did not answer within 5 seconds".into())
            }
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
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(w) = self.worker.take() {
            let _ = w.join();
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
    use super::{bgrx_to_bgra, unpack_size, Device, Shared};
    use std::sync::atomic::Ordering;
    use std::sync::mpsc::Sender;
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

    /// The size and row pitch of the reader's current RGB32 type.
    unsafe fn mode_of(reader: &IMFSourceReader) -> Result<((u32, u32), i32), String> {
        let t = reader
            .GetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32)
            .map_err(|e| e.to_string())?;
        let (w, h) = unpack_size(t.GetUINT64(&MF_MT_FRAME_SIZE).map_err(|e| e.to_string())?);
        let stride = t.GetUINT32(&MF_MT_DEFAULT_STRIDE).map_or((w * 4) as i32, |v| v as i32);
        Ok(((w, h), stride))
    }

    /// The worker: opens the device, reports its mode (or the error) on `tx`, then reads samples until asked to stop
    /// or the stream ends.
    pub fn run(link: &str, shared: &Shared, tx: &Sender<Result<(u32, u32), String>>) {
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
            let _ = tx.send(Err("Media Foundation is not available".into()));
            return;
        };
        // SAFETY: Media Foundation is started on this thread; every COM object below lives and dies here.
        unsafe {
            let (source, reader) = match open_reader(link) {
                Ok(v) => v,
                Err(e) => {
                    let _ = tx.send(Err(e));
                    return;
                }
            };
            let (mut size, mut stride) = match mode_of(&reader) {
                Ok(v) => v,
                Err(e) => {
                    let _ = tx.send(Err(e));
                    return;
                }
            };
            let _ = tx.send(Ok(size));
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
                let converted = bgrx_to_bgra(bytes, size.0, size.1, stride);
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
            size: (1, 1),
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
            size: (2, 1),
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
        let mut frame = Frame::default();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !cap.take_frame(&mut frame) {
            assert!(std::time::Instant::now() < deadline, "no frame within 5 seconds");
            std::thread::sleep(Duration::from_millis(10));
        }
        eprintln!("frame {}x{}, mode {:?}", frame.width, frame.height, cap.size);
        assert_eq!((frame.width, frame.height), cap.size);
        assert_eq!(frame.data.len(), (frame.width * frame.height * 4) as usize);
    }
}
