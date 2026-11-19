//! Window capture with Windows.Graphics.Capture.
//!
//! The OS hands us a Direct3D 11 texture holding the composed pixels of the target window. This
//! first version copies the client area into a CPU buffer (CAP-2, CPU copy path), which the
//! renderer uploads to Vulkan. A shared-handle path can replace it later.

use std::{
    ffi::c_void,
    mem::size_of,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
};

use windows::{
    core::{factory, IInspectable, Interface, Result},
    Foundation::TypedEventHandler,
    Graphics::{
        Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession},
        DirectX::{Direct3D11::IDirect3DDevice, DirectXPixelFormat},
    },
    Win32::{
        Foundation::{HMODULE, HWND, POINT, RECT},
        Graphics::{
            Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN},
            Direct3D11::*,
            Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS},
            Dxgi::{Common::*, CreateDXGIFactory1, IDXGIAdapter, IDXGIAdapter1, IDXGIDevice, IDXGIFactory1},
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

/// One captured frame: tightly packed BGRA8, top-down.
#[derive(Default)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
    pub seq: u64,
}

struct Shared {
    frame: Mutex<Frame>,
    closed: AtomicBool,
    captured: AtomicU64,
}

pub struct Capture {
    shared: Arc<Shared>,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    pub adapter: String,
    /// LUID of that adapter (T-03).
    pub adapter_luid: Luid,
}

impl Capture {
    /// Starts capturing `hwnd`. With `crop`, only the client area is kept (no title bar, no borders). The
    /// Direct3D 11 device is created on the adapter with the LUID `on` (the renderer's, HW-3), else on the
    /// default adapter; [`Capture::adapter_luid`] tells which one it got.
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
            frame: Mutex::new(Frame::default()),
            closed: AtomicBool::new(false),
            captured: AtomicU64::new(0),
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

        {
            let worker = Mutex::new(Worker {
                shared: shared.clone(),
                hwnd_value: hwnd.0 as isize,
                crop,
                device: device.clone(),
                context,
                direct3d_device: direct3d_device.clone(),
                format,
                staging: None,
                pool_size: (size.Width, size.Height),
            });
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
            pool,
            session,
            adapter,
            adapter_luid,
        })
    }

    /// Copies the newest frame into `into` if it is newer than `into.seq`. Returns true if it did.
    pub fn take_frame(&self, into: &mut Frame) -> bool {
        let mut f = self.shared.frame.lock().unwrap();
        if f.seq == into.seq || f.data.is_empty() {
            return false;
        }
        std::mem::swap(&mut f.data, &mut into.data);
        into.width = f.width;
        into.height = f.height;
        into.seq = f.seq;
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

/// State of the frame callback. The Direct3D objects are only used from the callback, and the
/// device is multithread protected, so moving them to the capture thread is sound.
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

        let (cx, cy, cw, ch) = if self.crop {
            client_crop(HWND(self.hwnd_value as *mut c_void), desc.Width, desc.Height)
        } else {
            (
                0,
                0,
                desc.Width.min(content.Width as u32),
                desc.Height.min(content.Height as u32),
            )
        };
        if cw > 0 && ch > 0 {
            if self.staging.as_ref().map(|s| (s.1, s.2)) != Some((cw, ch)) {
                self.staging = Some((create_staging(&self.device, cw, ch)?, cw, ch));
            }
            let tex = &self.staging.as_ref().unwrap().0;
            let region = D3D11_BOX {
                left: cx,
                top: cy,
                front: 0,
                right: cx + cw,
                bottom: cy + ch,
                back: 1,
            };
            unsafe {
                self.context
                    .CopySubresourceRegion(tex, 0, 0, 0, 0, &texture, 0, Some(&region));
                let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                self.context.Map(tex, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
                {
                    let mut out = self.shared.frame.lock().unwrap();
                    let row_bytes = cw as usize * 4;
                    out.data.resize(row_bytes * ch as usize, 0);
                    for y in 0..ch as usize {
                        let src = (mapped.pData as *const u8).add(y * mapped.RowPitch as usize);
                        std::ptr::copy_nonoverlapping(src, out.data.as_mut_ptr().add(y * row_bytes), row_bytes);
                    }
                    out.width = cw;
                    out.height = ch;
                    out.seq += 1;
                }
                self.context.Unmap(tex, 0);
            }
            self.shared.captured.fetch_add(1, Ordering::Relaxed);
        }

        // The window was resized: the pool must follow or frames get cropped or stretched.
        if (content.Width, content.Height) != self.pool_size && content.Width > 0 && content.Height > 0 {
            self.pool_size = (content.Width, content.Height);
            pool.Recreate(&self.direct3d_device, self.format, 2, content)?;
        }
        Ok(())
    }
}

/// Name and LUID of the adapter the capture's Direct3D 11 device is created on when asked for `on` (see
/// [`create_device`]), found by creating and dropping such a device: no window, no capture session.
pub fn capture_adapter(on: Option<Luid>) -> Result<(String, Luid)> {
    let (_device, _context, name, luid) = create_device(on)?;
    Ok((name, luid))
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

/// Client area of the window as (x, y, w, h) inside the captured texture, clamped to it.
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
        let x = (origin.x - frame.left).max(0) as u32;
        let y = (origin.y - frame.top).max(0) as u32;
        let w = (client.right - client.left).max(0) as u32;
        let h = (client.bottom - client.top).max(0) as u32;
        let x = x.min(tex_w);
        let y = y.min(tex_h);
        (x, y, w.min(tex_w - x), h.min(tex_h - y))
    }
}
