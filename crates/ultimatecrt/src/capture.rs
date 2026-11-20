//! Window capture with Windows.Graphics.Capture.
//!
//! The OS hands us a Direct3D 11 texture holding the composed pixels of the target window. Two paths take the
//! client area to the renderer (ARCHITECTURE A3, A29, QUESTIONS CAP-2):
//! - **CPU copy** (default, [`CapturePath::Cpu`]): the client area is copied into a staging texture, mapped, and
//!   its rows copied into a CPU buffer ([`Frame::data`]) that the renderer uploads to Vulkan.
//! - **Zero-copy** ([`CapturePath::ZeroCopy`], experimental): the client area is copied on the GPU into one of
//!   [`POOL_SLOTS`] shared textures ([`SharedTextures`], created on the capture device, which is on the renderer's
//!   adapter, HW-3); the capture thread waits for that copy on the GPU (an event query, no pixel crosses the CPU)
//!   and publishes the slot ([`SharedFrame`]); the renderer imports the textures once and samples them directly.
//!   For the CPU analyses (integer-scale detection, picture area, black frames) the client area is still read
//!   back, but only as a sample: on the first frame of a size, at most every [`SAMPLE_EVERY`] otherwise, and on
//!   every frame while [`Capture::set_full_rate_samples`] asks for it (a detection is running).
//!
//! Any zero-copy failure on the capture side (texture or handle creation, a copy that does not finish) switches
//! the session to the CPU copy at once, for that frame too, and is reported once ([`Capture::take_failure`]).

use std::{
    ffi::c_void,
    mem::size_of,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use windows::{
    core::{factory, IInspectable, Interface, Result, PCWSTR},
    Foundation::TypedEventHandler,
    Graphics::{
        Capture::{Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession},
        DirectX::{Direct3D11::IDirect3DDevice, DirectXPixelFormat},
    },
    Win32::{
        Foundation::{CloseHandle, E_FAIL, HANDLE, HMODULE, HWND, POINT, RECT},
        Graphics::{
            Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN},
            Direct3D11::*,
            Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS},
            Dxgi::{
                Common::*, CreateDXGIFactory1, IDXGIAdapter, IDXGIAdapter1, IDXGIDevice, IDXGIFactory1, IDXGIResource,
                IDXGIResource1, DXGI_SHARED_RESOURCE_READ, DXGI_SHARED_RESOURCE_WRITE,
            },
            Gdi::ClientToScreen,
        },
        System::WinRT::{
            Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess},
            Graphics::Capture::IGraphicsCaptureItemInterop,
        },
        UI::WindowsAndMessaging::GetClientRect,
    },
};

use crate::adapters::Luid;
use crate::zerocopy::{clamp_crop, CopyCost, SlotPool};
pub use crate::zerocopy::{CapturePath, HandleKind, POOL_SLOTS};

/// The longest gap between two CPU samples of the zero-copy path (the black-frame scan samples at the same rate,
/// `lifecycle::Health`).
pub const SAMPLE_EVERY: Duration = Duration::from_millis(100);

/// How long the capture thread waits for its GPU copy before it gives the zero-copy path up (a healthy copy takes
/// microseconds; a hung GPU is the renderer's to report).
const COPY_TIMEOUT: Duration = Duration::from_secs(1);

/// One captured frame: tightly packed BGRA8, top-down.
#[derive(Default)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
    pub seq: u64,
    /// Zero-copy: the shared texture holding this frame (the renderer samples it), `None` on the CPU path.
    pub shared: Option<SharedFrame>,
    /// Zero-copy: `data` is an earlier sample of the same size, not this frame's pixels (the CPU analyses skip
    /// it). Always false on the CPU path.
    pub pixels_old: bool,
}

/// The shared state of the frame callback and the main thread.
struct Shared {
    frame: Mutex<Latest>,
    closed: AtomicBool,
    captured: AtomicU64,
    /// The path asked ([`CapturePath`] as `u8`): read by the capture thread on every frame.
    path: AtomicU8,
    /// Sample every frame for the CPU analyses (zero-copy path).
    full_rate: AtomicBool,
    /// Why the capture thread left the zero-copy path, until [`Capture::take_failure`].
    failure: Mutex<Option<String>>,
    /// Copy cost per frame on the capture thread (both paths).
    cost: Mutex<CopyCost>,
}

/// The newest frame as the capture thread left it.
#[derive(Default)]
struct Latest {
    width: u32,
    height: u32,
    seq: u64,
    /// The CPU pixels (every frame on the CPU path, the samples on the zero-copy path).
    data: Vec<u8>,
    /// The frame `data` belongs to.
    data_seq: u64,
    /// Zero-copy: the pool the newest frame was published to.
    pool: Option<Arc<SharedTextures>>,
}

fn path_of(v: u8) -> CapturePath {
    if v == 1 {
        CapturePath::ZeroCopy
    } else {
        CapturePath::Cpu
    }
}

pub struct Capture {
    shared: Arc<Shared>,
    worker: Arc<Mutex<Worker>>,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    pub adapter: String,
    /// LUID of that adapter (T-03).
    pub adapter_luid: Luid,
}

impl Capture {
    /// Starts capturing `hwnd`. With `crop`, only the client area is kept (no title bar, no borders). The
    /// Direct3D 11 device is created on the adapter with the LUID `on` (the renderer's, HW-3), else on the
    /// default adapter; [`Capture::adapter_luid`] tells which one it got. The CPU copy runs until
    /// [`Capture::set_path`] asks for zero-copy.
    pub fn start(hwnd: HWND, crop: bool, on: Option<Luid>) -> Result<Self> {
        let (device, context, adapter, adapter_luid) = create_device(on)?;
        let direct3d_device = to_winrt_device(&device)?;

        let interop = factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
        let item: GraphicsCaptureItem = unsafe { interop.CreateForWindow(hwnd)? };
        let size = item.Size()?;

        let format = DirectXPixelFormat::B8G8R8A8UIntNormalized;
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(&direct3d_device, format, 2, size)?;
        let session = pool.CreateCaptureSession(&item)?;
        let _ = session.SetIsCursorCaptureEnabled(false);
        // Needs a recent Windows build and may be refused; the yellow border then stays.
        let _ = session.SetIsBorderRequired(false);

        let shared = Arc::new(Shared {
            frame: Mutex::new(Latest::default()),
            closed: AtomicBool::new(false),
            captured: AtomicU64::new(0),
            path: AtomicU8::new(0),
            full_rate: AtomicBool::new(false),
            failure: Mutex::new(None),
            cost: Mutex::new(CopyCost::default()),
        });

        {
            let shared = shared.clone();
            item.Closed(&TypedEventHandler::<GraphicsCaptureItem, IInspectable>::new(
                move |_, _| {
                    shared.closed.store(true, Ordering::Release);
                    Ok(())
                },
            ))?;
        }

        let worker = Arc::new(Mutex::new(Worker {
            shared: shared.clone(),
            hwnd_value: hwnd.0 as isize,
            crop,
            device: device.clone(),
            context,
            direct3d_device: direct3d_device.clone(),
            format,
            staging: None,
            pool_size: (size.Width, size.Height),
            shared_pool: None,
            generation: 0,
            last_sample: None,
            pending: None,
        }));
        {
            let worker = worker.clone();
            pool.FrameArrived(&TypedEventHandler::<Direct3D11CaptureFramePool, IInspectable>::new(
                move |pool, _| match pool.as_ref() {
                    Some(pool) => worker.lock().unwrap().on_frame(pool),
                    None => Ok(()),
                },
            ))?;
        }

        session.StartCapture()?;
        Ok(Self {
            shared,
            worker,
            pool,
            session,
            adapter,
            adapter_luid,
        })
    }

    /// Asks for a capture path from the next frame on (restart-free on the capture side).
    pub fn set_path(&self, path: CapturePath) {
        self.shared
            .path
            .store(u8::from(path == CapturePath::ZeroCopy), Ordering::Release);
    }

    /// The path the capture thread runs now (the CPU copy after a failure).
    pub fn path(&self) -> CapturePath {
        path_of(self.shared.path.load(Ordering::Acquire))
    }

    /// Zero-copy: sample every frame for the CPU analyses (while a detection runs), else at most every
    /// [`SAMPLE_EVERY`].
    pub fn set_full_rate_samples(&self, on: bool) {
        self.shared.full_rate.store(on, Ordering::Relaxed);
    }

    /// Why the capture thread left the zero-copy path (once), `None` when it did not.
    pub fn take_failure(&self) -> Option<String> {
        self.shared.failure.lock().unwrap().take()
    }

    /// The capture thread's copy cost since the last call: (mean ms, max ms, frames).
    pub fn take_copy_cost(&self) -> Option<(f64, f64, u64)> {
        self.shared.cost.lock().unwrap().take()
    }

    /// Copies the newest frame into `into` if it is newer than `into.seq`. Returns true if it did. On the
    /// zero-copy path `into.shared` holds the frame's shared texture and `into.data` the latest CPU sample
    /// (`into.pixels_old` when it is not this frame's).
    pub fn take_frame(&self, into: &mut Frame) -> bool {
        // A frame kept for want of a free slot is copied as soon as one is (the render side released it).
        if let Ok(mut w) = self.worker.try_lock() {
            w.retry_pending();
        }
        let mut f = self.shared.frame.lock().unwrap();
        if f.seq == into.seq || f.seq == 0 {
            return false;
        }
        // Zero-copy: the newest published slot. None while the capture overwrites the only ready slot with a newer
        // frame (it publishes it in a moment): nothing new yet.
        let shared = match &f.pool {
            Some(p) => match p.take(0) {
                Some(s) => Some(s),
                None => return false,
            },
            None => None,
        };
        into.width = f.width;
        into.height = f.height;
        into.seq = f.seq;
        into.shared = shared;
        if f.data_seq != 0 {
            // The pixels not given out yet (every frame on the CPU path, a sample on the zero-copy path). The
            // caller's old buffer goes back to be refilled.
            std::mem::swap(&mut f.data, &mut into.data);
            into.pixels_old = f.data_seq != f.seq;
            f.data_seq = 0;
        } else {
            into.pixels_old = true;
        }
        true
    }

    pub fn is_closed(&self) -> bool {
        self.shared.closed.load(Ordering::Acquire)
    }

    pub fn frames_captured(&self) -> u64 {
        self.shared.captured.load(Ordering::Relaxed)
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}

/// A frame kept because every shared slot was in use: the capture frame object (so the pool does not reuse its
/// buffer), its texture and the client area.
struct Pending {
    _frame: Direct3D11CaptureFrame,
    texture: ID3D11Texture2D,
    crop: (u32, u32, u32, u32),
}

/// State of the frame callback. The Direct3D objects are used from the callback and, for a pending frame, from
/// [`Capture::take_frame`] (behind the same mutex); the device is multithread protected, so moving them to the
/// capture thread is sound.
struct Worker {
    shared: Arc<Shared>,
    hwnd_value: isize,
    crop: bool,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    direct3d_device: IDirect3DDevice,
    format: DirectXPixelFormat,
    staging: Option<(ID3D11Texture2D, u32, u32)>,
    pool_size: (i32, i32),
    /// Zero-copy: the pool of the current client size.
    shared_pool: Option<Arc<SharedTextures>>,
    /// Pools made so far (the generation of the next one).
    generation: u64,
    /// When the last CPU sample of the zero-copy path was taken.
    last_sample: Option<Instant>,
    pending: Option<Pending>,
}

unsafe impl Send for Worker {}

impl Worker {
    fn on_frame(&mut self, pool: &Direct3D11CaptureFramePool) -> Result<()> {
        let Ok(frame) = pool.TryGetNextFrame() else {
            return Ok(());
        };
        let content = frame.ContentSize()?;
        let surface = frame.Surface()?;
        let access: IDirect3DDxgiInterfaceAccess = surface.cast()?;
        let texture: ID3D11Texture2D = unsafe { access.GetInterface()? };
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { texture.GetDesc(&mut desc) };

        let crop = if self.crop {
            client_crop(HWND(self.hwnd_value as *mut c_void), desc.Width, desc.Height)
        } else {
            (
                0,
                0,
                desc.Width.min(content.Width as u32),
                desc.Height.min(content.Height as u32),
            )
        };
        // A newer frame replaces a pending one.
        self.pending = None;
        if crop.2 > 0 && crop.3 > 0 {
            if path_of(self.shared.path.load(Ordering::Acquire)) == CapturePath::ZeroCopy {
                if self.zero_copy(&texture, crop).is_some() {
                    self.pending = Some(Pending {
                        _frame: frame,
                        texture,
                        crop,
                    });
                }
            } else {
                self.shared_pool = None;
                self.cpu_frame(&texture, crop)?;
            }
        }

        // The window was resized: the pool must follow or frames get cropped or stretched.
        if (content.Width, content.Height) != self.pool_size && content.Width > 0 && content.Height > 0 {
            self.pool_size = (content.Width, content.Height);
            pool.Recreate(&self.direct3d_device, self.format, 2, content)?;
        }
        Ok(())
    }

    /// The CPU path: copies the client area to the staging texture, maps it and copies its rows into the frame.
    fn cpu_frame(&mut self, texture: &ID3D11Texture2D, crop: (u32, u32, u32, u32)) -> Result<()> {
        let t0 = Instant::now();
        let shared = self.shared.clone();
        let mut out = shared.frame.lock().unwrap();
        read_back_region(
            &self.device,
            &self.context,
            &mut self.staging,
            texture,
            crop,
            &mut out.data,
        )?;
        out.width = crop.2;
        out.height = crop.3;
        out.seq += 1;
        out.data_seq = out.seq;
        out.pool = None;
        drop(out);
        self.shared.cost.lock().unwrap().push(t0.elapsed().as_secs_f64() * 1e6);
        self.shared.captured.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Copies the client area `crop` of `texture` into `data` (tightly packed BGRA8) through the staging texture.
    fn read_back(&mut self, texture: &ID3D11Texture2D, crop: (u32, u32, u32, u32), data: &mut Vec<u8>) -> Result<()> {
        read_back_region(&self.device, &self.context, &mut self.staging, texture, crop, data)
    }

    /// The zero-copy path for one frame. Returns `Some(())` when every slot is in use (the caller keeps the
    /// frame, [`Worker::retry_pending`]). Any failure falls back to the CPU path for this frame and the session.
    fn zero_copy(&mut self, texture: &ID3D11Texture2D, crop: (u32, u32, u32, u32)) -> Option<()> {
        match self.try_zero_copy(texture, crop) {
            Ok(true) => None,
            Ok(false) => Some(()),
            Err(e) => {
                self.fail(&e.to_string());
                if let Err(e) = self.cpu_frame(texture, crop) {
                    eprintln!("warning: capture: the CPU copy failed too: {e}");
                }
                None
            }
        }
    }

    /// Leaves the zero-copy path for the CPU copy, with the reason for [`Capture::take_failure`].
    fn fail(&mut self, why: &str) {
        // The reason first: whoever reads the path back as zero-copy and asks again sees the failure as well.
        {
            let mut f = self.shared.failure.lock().unwrap();
            if f.is_none() {
                *f = Some(format!("capture side: {why}"));
            }
        }
        self.shared.path.store(0, Ordering::Release);
        self.shared_pool = None;
        self.pending = None;
    }

    /// Copies the client area into a free slot of the pool (made for this size when needed), waits for the copy
    /// on the GPU, then publishes it with the CPU sample when one is due. Ok(false): no free slot.
    fn try_zero_copy(&mut self, texture: &ID3D11Texture2D, crop: (u32, u32, u32, u32)) -> Result<bool> {
        let (_, _, cw, ch) = crop;
        let resized = self.shared_pool.as_ref().is_none_or(|p| p.size() != (cw, ch));
        if resized {
            self.generation += 1;
            self.shared_pool = Some(
                SharedTextures::create(&self.device, (cw, ch), HandleKind::Nt, self.generation)
                    .or_else(|_| SharedTextures::create(&self.device, (cw, ch), HandleKind::Kmt, self.generation))?,
            );
        }
        let pool = self.shared_pool.clone().expect("pool made above");
        let t0 = Instant::now();
        let Some(slot) = pool.begin_write() else {
            return Ok(false);
        };
        if let Err(e) = pool.copy_into(&self.context, slot, texture, (crop.0, crop.1)) {
            pool.abort_write(slot);
            return Err(e);
        }
        let cost = t0.elapsed().as_secs_f64() * 1e6;
        let due = resized
            || self.shared.full_rate.load(Ordering::Relaxed)
            || self.last_sample.is_none_or(|t| t.elapsed() >= SAMPLE_EVERY);
        let mut sample = Vec::new();
        if due {
            // The previous sample's buffer when the main thread gave it back, else a new one.
            sample = std::mem::take(&mut self.shared.frame.lock().unwrap().data);
            if let Err(e) = self.read_back(texture, crop, &mut sample) {
                pool.abort_write(slot);
                return Err(e);
            }
            self.last_sample = Some(Instant::now());
        }
        let mut out = self.shared.frame.lock().unwrap();
        let seq = out.seq + 1;
        if !pool.publish(slot, seq) {
            return Err(windows::core::Error::new(
                E_FAIL,
                "a shared slot changed state while it was written",
            ));
        }
        out.seq = seq;
        out.width = cw;
        out.height = ch;
        if due {
            out.data = sample;
            out.data_seq = seq;
        }
        out.pool = Some(pool);
        drop(out);
        self.shared.cost.lock().unwrap().push(cost);
        self.shared.captured.fetch_add(1, Ordering::Relaxed);
        Ok(true)
    }

    /// Copies the frame kept for want of a slot, when one is free now.
    fn retry_pending(&mut self) {
        let Some(p) = self.pending.take() else { return };
        if path_of(self.shared.path.load(Ordering::Acquire)) != CapturePath::ZeroCopy {
            return;
        }
        if self.zero_copy(&p.texture, p.crop).is_some() {
            self.pending = Some(p);
        }
    }
}

/// The client area as a copy box.
fn crop_box((x, y, w, h): (u32, u32, u32, u32)) -> D3D11_BOX {
    D3D11_BOX {
        left: x,
        top: y,
        front: 0,
        right: x + w,
        bottom: y + h,
        back: 1,
    }
}

/// The shared textures of one client size (zero-copy, A29): [`POOL_SLOTS`] BGRA8 textures on the capture device,
/// each with a shared handle the renderer imports, the event query of the copy, and the slot states
/// ([`SlotPool`]). A new size makes a new pool (a new `generation`); the old one lives until the renderer dropped
/// every [`SharedFrame`] of it.
pub struct SharedTextures {
    generation: u64,
    size: (u32, u32),
    kind: HandleKind,
    textures: Vec<ID3D11Texture2D>,
    handles: Vec<HANDLE>,
    query: ID3D11Query,
    slots: Mutex<SlotPool>,
}

// SAFETY: the textures and the query are used only by the thread that holds the capture `Worker` (the copy) on a
// multithread-protected device; the render side reads only `handles`, `size`, `kind` and `generation` (plain
// values) and the slot states (behind their mutex).
unsafe impl Send for SharedTextures {}
unsafe impl Sync for SharedTextures {}

impl SharedTextures {
    /// Makes the pool of `size` on `device` with handles of `kind` (`D3D11_RESOURCE_MISC_SHARED_NTHANDLE` and
    /// `SHARED` for an NT handle, `SHARED` alone for a KMT handle).
    pub fn create(device: &ID3D11Device, size: (u32, u32), kind: HandleKind, generation: u64) -> Result<Arc<Self>> {
        if size.0 == 0 || size.1 == 0 {
            return Err(windows::core::Error::new(E_FAIL, "a shared texture of 0 size"));
        }
        let misc = match kind {
            HandleKind::Nt => D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0 | D3D11_RESOURCE_MISC_SHARED.0,
            HandleKind::Kmt => D3D11_RESOURCE_MISC_SHARED.0,
        };
        let desc = D3D11_TEXTURE2D_DESC {
            Width: size.0,
            Height: size.1,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: misc as u32,
        };
        let mut textures = Vec::with_capacity(POOL_SLOTS);
        let mut handles: Vec<HANDLE> = Vec::with_capacity(POOL_SLOTS);
        let close = |handles: &[HANDLE]| {
            if kind == HandleKind::Nt {
                for h in handles {
                    // SAFETY: NT handles made by CreateSharedHandle below, closed once.
                    unsafe {
                        let _ = CloseHandle(*h);
                    }
                }
            }
        };
        for _ in 0..POOL_SLOTS {
            let made = (|| -> Result<(ID3D11Texture2D, HANDLE)> {
                let mut tex = None;
                // SAFETY: plain COM calls on a live device with a valid description.
                unsafe {
                    device.CreateTexture2D(&desc, None, Some(&mut tex))?;
                    let tex = tex.ok_or_else(|| windows::core::Error::new(E_FAIL, "no texture"))?;
                    let handle = match kind {
                        HandleKind::Nt => tex.cast::<IDXGIResource1>()?.CreateSharedHandle(
                            None,
                            DXGI_SHARED_RESOURCE_READ.0 | DXGI_SHARED_RESOURCE_WRITE.0,
                            PCWSTR::null(),
                        )?,
                        HandleKind::Kmt => tex.cast::<IDXGIResource>()?.GetSharedHandle()?,
                    };
                    Ok((tex, handle))
                }
            })();
            match made {
                Ok((t, h)) => {
                    textures.push(t);
                    handles.push(h);
                }
                Err(e) => {
                    close(&handles);
                    return Err(e);
                }
            }
        }
        let mut query = None;
        let q = D3D11_QUERY_DESC {
            Query: D3D11_QUERY_EVENT,
            MiscFlags: 0,
        };
        // SAFETY: as above.
        if let Err(e) = unsafe { device.CreateQuery(&q, Some(&mut query)) } {
            close(&handles);
            return Err(e);
        }
        Ok(Arc::new(Self {
            generation,
            size,
            kind,
            textures,
            handles,
            query: query.expect("CreateQuery succeeded"),
            slots: Mutex::new(SlotPool::default()),
        }))
    }

    /// A pool whose handles are not shared textures (failure-injection tests of the renderer's import): the
    /// import must fail cleanly. Its textures are ordinary ones.
    #[doc(hidden)]
    pub fn with_bogus_handles(device: &ID3D11Device, size: (u32, u32), generation: u64) -> Result<Arc<Self>> {
        let p = Self::create(device, size, HandleKind::Kmt, generation)?;
        let mut p = Arc::into_inner(p).expect("only reference");
        p.handles.fill(HANDLE(0x7FFF_0000usize as *mut c_void));
        Ok(Arc::new(p))
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    pub fn kind(&self) -> HandleKind {
        self.kind
    }

    /// The shared handle of every slot, as integers (the renderer imports them).
    pub fn handles(&self) -> Vec<isize> {
        self.handles.iter().map(|h| h.0 as isize).collect()
    }

    /// The Direct3D 11 texture of `slot` (tests fill it directly).
    pub fn texture(&self, slot: usize) -> &ID3D11Texture2D {
        &self.textures[slot]
    }

    /// A slot to write ([`SlotPool::begin_write`]).
    pub fn begin_write(&self) -> Option<usize> {
        self.slots.lock().unwrap().begin_write()
    }

    /// The write into `slot` failed ([`SlotPool::abort_write`]).
    pub fn abort_write(&self, slot: usize) {
        self.slots.lock().unwrap().abort_write(slot);
    }

    /// The write into `slot` is complete on the GPU: it holds frame `seq` ([`SlotPool::publish`]).
    pub fn publish(&self, slot: usize, seq: u64) -> bool {
        self.slots.lock().unwrap().publish(slot, seq)
    }

    /// The slot states, for tests and diagnostics.
    pub fn slot_states(&self) -> [crate::zerocopy::SlotState; POOL_SLOTS] {
        self.slots.lock().unwrap().states()
    }

    /// Copies the `self.size` region at `origin` of `src` into `slot` and waits until the copy finished on the
    /// GPU (synchronisation option A of A29: `End` of an event query, `Flush`, then poll `GetData`; the copy
    /// itself takes microseconds). After this the renderer may read the slot. `context` belongs to the device the
    /// pool was made on; `slot` is `Writing`.
    pub fn copy_into(
        &self,
        context: &ID3D11DeviceContext,
        slot: usize,
        src: &ID3D11Texture2D,
        origin: (u32, u32),
    ) -> Result<()> {
        let region = crop_box((origin.0, origin.1, self.size.0, self.size.1));
        // SAFETY: both textures live on the context's device; the region lies inside `src` (the caller's crop is
        // clamped to it) and the destination is exactly the region's size.
        unsafe {
            context.CopySubresourceRegion(&self.textures[slot], 0, 0, 0, 0, src, 0, Some(&region));
        }
        self.wait_gpu(context)
    }

    /// Waits for every command issued so far on `context` to finish on the GPU (the event query).
    pub fn wait_gpu(&self, context: &ID3D11DeviceContext) -> Result<()> {
        let start = Instant::now();
        // SAFETY: the query belongs to the context's device; `done` outlives the calls.
        unsafe {
            context.End(&self.query);
            context.Flush();
            loop {
                let mut done = 0i32;
                context.GetData(
                    &self.query,
                    Some((&mut done as *mut i32).cast()),
                    size_of::<i32>() as u32,
                    0,
                )?;
                if done != 0 {
                    return Ok(());
                }
                if start.elapsed() > COPY_TIMEOUT {
                    return Err(windows::core::Error::new(
                        E_FAIL,
                        "the GPU copy into the shared texture did not finish within 1 s",
                    ));
                }
                std::thread::yield_now();
            }
        }
    }

    /// Render side: the newest frame published after `after`, held until every clone of it is dropped.
    pub fn take(self: &Arc<Self>, after: u64) -> Option<SharedFrame> {
        let (slot, seq) = self.slots.lock().unwrap().take(after)?;
        Some(SharedFrame {
            hold: Arc::new(Hold {
                pool: self.clone(),
                slot,
                seq,
            }),
        })
    }
}

impl Drop for SharedTextures {
    fn drop(&mut self) {
        if self.kind == HandleKind::Nt {
            for h in &self.handles {
                // SAFETY: the NT handles this pool made, closed once. A Vulkan import keeps its own reference.
                unsafe {
                    let _ = CloseHandle(*h);
                }
            }
        }
    }
}

/// One published frame in a shared texture (zero-copy). Clones share one hold on the slot; the slot is released
/// to the capture when the last clone is dropped (the renderer keeps one while a frame in flight reads it).
#[derive(Clone)]
pub struct SharedFrame {
    hold: Arc<Hold>,
}

struct Hold {
    pool: Arc<SharedTextures>,
    slot: usize,
    seq: u64,
}

impl Drop for Hold {
    fn drop(&mut self) {
        self.pool.slots.lock().unwrap().release(self.slot);
    }
}

impl SharedFrame {
    pub fn slot(&self) -> usize {
        self.hold.slot
    }

    pub fn seq(&self) -> u64 {
        self.hold.seq
    }

    pub fn pool(&self) -> &Arc<SharedTextures> {
        &self.hold.pool
    }

    pub fn generation(&self) -> u64 {
        self.hold.pool.generation
    }

    pub fn size(&self) -> (u32, u32) {
        self.hold.pool.size
    }
}

impl std::fmt::Debug for SharedFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "SharedFrame(generation {}, slot {}, seq {})",
            self.generation(),
            self.slot(),
            self.seq()
        )
    }
}

/// Name and LUID of the adapter the capture's Direct3D 11 device is created on when asked for `on` (see
/// [`create_device`]), found by creating and dropping such a device: no window, no capture session.
pub fn capture_adapter(on: Option<Luid>) -> Result<(String, Luid)> {
    let (_device, _context, name, luid) = create_device(on)?;
    Ok((name, luid))
}

/// A Direct3D 11 device as the capture makes it (on the adapter with the LUID `on`, multithread protected), with
/// its immediate context and its adapter's LUID: the zero-copy tests and measurements use it without a window.
pub fn d3d11_device(on: Option<Luid>) -> Result<(ID3D11Device, ID3D11DeviceContext, Luid)> {
    let (device, context, _, luid) = create_device(on)?;
    Ok((device, context, luid))
}

/// The DXGI adapter with this LUID (`EnumAdapters1`), `None` when no adapter has it.
fn adapter_by_luid(luid: Luid) -> Result<Option<IDXGIAdapter1>> {
    // SAFETY: plain COM calls on a factory created here.
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
        let mut i = 0;
        while let Ok(adapter) = factory.EnumAdapters1(i) {
            if Luid::from_windows(adapter.GetDesc1()?.AdapterLuid) == luid {
                return Ok(Some(adapter));
            }
            i += 1;
        }
    }
    Ok(None)
}

/// Creates the capture's Direct3D 11 device on the adapter with the LUID `on` (HW-3: the adapter the Vulkan
/// renderer runs on), else on the default adapter. A LUID that no adapter has prints a warning and uses the
/// default adapter; the caller sees it in the returned LUID.
fn create_device(on: Option<Luid>) -> Result<(ID3D11Device, ID3D11DeviceContext, String, Luid)> {
    let adapter = match on {
        Some(l) => {
            let a = adapter_by_luid(l)?;
            if a.is_none() {
                eprintln!("warning: capture: no DXGI adapter with LUID {l}; using the default adapter");
            }
            a
        }
        None => None,
    };
    let mut device = None;
    let mut context = None;
    unsafe {
        D3D11CreateDevice(
            adapter.as_ref().map(|a| a.cast::<IDXGIAdapter>()).transpose()?.as_ref(),
            // An explicit adapter needs the unknown driver type (D3D11CreateDevice contract).
            if adapter.is_some() {
                D3D_DRIVER_TYPE_UNKNOWN
            } else {
                D3D_DRIVER_TYPE_HARDWARE
            },
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )?;
    }
    let device = device.unwrap();
    let context = context.unwrap();
    // The free-threaded frame callback and the creating thread share this device.
    if let Ok(mt) = device.cast::<ID3D11Multithread>() {
        unsafe {
            let _ = mt.SetMultithreadProtected(true);
        }
    }
    let dxgi: IDXGIDevice = device.cast()?;
    let adapter = unsafe { dxgi.GetAdapter()?.GetDesc()? };
    let name = String::from_utf16_lossy(&adapter.Description)
        .trim_end_matches('\0')
        .to_string();
    Ok((device, context, name, Luid::from_windows(adapter.AdapterLuid)))
}

fn to_winrt_device(device: &ID3D11Device) -> Result<IDirect3DDevice> {
    let dxgi: IDXGIDevice = device.cast()?;
    let inspectable = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi)? };
    inspectable.cast()
}

fn create_staging(device: &ID3D11Device, width: u32, height: u32) -> Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
    };
    let mut tex = None;
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut tex))? };
    Ok(tex.unwrap())
}

/// The CPU path's copy of a client area, for measurements and tests without a window (the same calls as the
/// capture thread: staging texture, `CopySubresourceRegion`, `Map`, row copy). `staging` is reused while its
/// size matches.
pub fn read_back_region(
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    staging: &mut Option<(ID3D11Texture2D, u32, u32)>,
    texture: &ID3D11Texture2D,
    crop: (u32, u32, u32, u32),
    data: &mut Vec<u8>,
) -> Result<()> {
    let (_, _, cw, ch) = crop;
    if staging.as_ref().map(|s| (s.1, s.2)) != Some((cw, ch)) {
        *staging = Some((create_staging(device, cw, ch)?, cw, ch));
    }
    let tex = &staging.as_ref().unwrap().0;
    let region = crop_box(crop);
    // SAFETY: as in `Worker::read_back`.
    unsafe {
        context.CopySubresourceRegion(tex, 0, 0, 0, 0, texture, 0, Some(&region));
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        context.Map(tex, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        let row_bytes = cw as usize * 4;
        data.resize(row_bytes * ch as usize, 0);
        for y in 0..ch as usize {
            let src = (mapped.pData as *const u8).add(y * mapped.RowPitch as usize);
            std::ptr::copy_nonoverlapping(src, data.as_mut_ptr().add(y * row_bytes), row_bytes);
        }
        context.Unmap(tex, 0);
    }
    Ok(())
}

/// Client area of the window as (x, y, w, h) inside the captured texture, clamped to it ([`clamp_crop`]).
/// The capture covers the window's extended frame (title bar and borders included, shadow excluded).
fn client_crop(hwnd: HWND, tex_w: u32, tex_h: u32) -> (u32, u32, u32, u32) {
    unsafe {
        let mut frame = RECT::default();
        let _ = DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut frame as *mut RECT as *mut c_void,
            size_of::<RECT>() as u32,
        );
        let mut client = RECT::default();
        let _ = GetClientRect(hwnd, &mut client);
        let mut origin = POINT { x: 0, y: 0 };
        let _ = ClientToScreen(hwnd, &mut origin);
        clamp_crop(
            i64::from(origin.x - frame.left),
            i64::from(origin.y - frame.top),
            i64::from(client.right - client.left),
            i64::from(client.bottom - client.top),
            (tex_w, tex_h),
        )
    }
}
