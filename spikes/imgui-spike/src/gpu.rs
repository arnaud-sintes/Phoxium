//! Vulkan context for the spike: a copy of `crates/ultimatecrt/src/gpu/context.rs` (T-09 headless
//! context with the counting validation messenger and synchronization validation), trimmed to what
//! the spike needs, plus small image and buffer helpers copied from `gpu/headless.rs` and
//! `gpu/mod.rs`. Copied, not shared: the spike must not depend on the main crate.

use std::ffi::{c_void, CStr};
use std::io::Write;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use ash::{ext, khr, vk, Device, Entry, Instance};

use crate::Res;

const VALIDATION_LAYER: &CStr = c"VK_LAYER_KHRONOS_validation";

/// True when `UCRT_REQUIRE_GPU` is set to a non-empty value other than `0`: a missing GPU or
/// validation layer then fails the GPU tests instead of skipping them.
pub fn gpu_required() -> bool {
    std::env::var("UCRT_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0")
}

/// True when the Vulkan loader is present and lists `VK_LAYER_KHRONOS_validation`.
pub fn validation_layer_available() -> bool {
    // SAFETY: loading the Vulkan loader library has no precondition beyond it being a real loader.
    let Ok(entry) = (unsafe { Entry::load() }) else {
        return false;
    };
    layer_present(&entry)
}

fn layer_present(entry: &Entry) -> bool {
    // SAFETY: global command, no handle involved.
    unsafe { entry.enumerate_instance_layer_properties() }
        .map(|layers| layers.iter().any(|l| l.layer_name_as_c_str() == Ok(VALIDATION_LAYER)))
        .unwrap_or(false)
}

fn layer_has_extension(entry: &Entry, name: &CStr) -> bool {
    // SAFETY: global command, no handle involved.
    unsafe { entry.enumerate_instance_extension_properties(Some(VALIDATION_LAYER)) }
        .map(|exts| exts.iter().any(|e| e.extension_name_as_c_str() == Ok(name)))
        .unwrap_or(false)
}

/// Shared count of validation and performance warnings and errors. Cloning shares the count.
#[derive(Clone, Default)]
pub struct ValidationCounter(Arc<AtomicU32>);

impl ValidationCounter {
    pub fn count(&self) -> u32 {
        self.0.load(Ordering::SeqCst)
    }
}

/// Debug-utils callback: prints every message to stderr and counts validation and performance
/// warnings and errors.
///
/// # Safety
/// Called by the Vulkan loader only: `data` is null or a valid callback-data struct, and `user` is
/// null or the `AtomicU32` of a [`ValidationCounter`] still owned by the context.
unsafe extern "system" fn on_message(
    severity: vk::DebugUtilsMessageSeverityFlagsEXT,
    types: vk::DebugUtilsMessageTypeFlagsEXT,
    data: *const vk::DebugUtilsMessengerCallbackDataEXT<'_>,
    user: *mut c_void,
) -> vk::Bool32 {
    let counted = types
        .intersects(vk::DebugUtilsMessageTypeFlagsEXT::VALIDATION | vk::DebugUtilsMessageTypeFlagsEXT::PERFORMANCE)
        && severity
            .intersects(vk::DebugUtilsMessageSeverityFlagsEXT::WARNING | vk::DebugUtilsMessageSeverityFlagsEXT::ERROR);
    if counted && !user.is_null() {
        (*(user as *const AtomicU32)).fetch_add(1, Ordering::SeqCst);
    }
    let msg = if data.is_null() || (*data).p_message.is_null() {
        std::borrow::Cow::Borrowed("")
    } else {
        CStr::from_ptr((*data).p_message).to_string_lossy()
    };
    let _ = writeln!(std::io::stderr(), "vulkan {severity:?} {types:?}: {msg}");
    vk::FALSE
}

fn messenger_info(counter: &ValidationCounter) -> vk::DebugUtilsMessengerCreateInfoEXT<'static> {
    vk::DebugUtilsMessengerCreateInfoEXT::default()
        .message_severity(vk::DebugUtilsMessageSeverityFlagsEXT::WARNING | vk::DebugUtilsMessageSeverityFlagsEXT::ERROR)
        .message_type(
            vk::DebugUtilsMessageTypeFlagsEXT::GENERAL
                | vk::DebugUtilsMessageTypeFlagsEXT::VALIDATION
                | vk::DebugUtilsMessageTypeFlagsEXT::PERFORMANCE,
        )
        .pfn_user_callback(Some(on_message))
        .user_data(Arc::as_ptr(&counter.0) as *mut c_void)
}

const NVIDIA: u32 = 0x10DE;

/// Vulkan 1.3 instance, device (dynamic rendering, synchronization2) and graphics queue.
/// Destroys the device and the instance in `Drop`: owners of child objects destroy them first.
pub struct Context {
    pub entry: Entry,
    pub instance: Instance,
    pub physical: vk::PhysicalDevice,
    pub device: Device,
    pub queue_family: u32,
    pub queue: vk::Queue,
    pub gpu_name: String,
    pub timestamp_period_ns: f32,
    pub timestamp_valid_bits: u32,
    messenger: Option<(ext::debug_utils::Instance, vk::DebugUtilsMessengerEXT)>,
    validation: ValidationCounter,
    sync_validation: bool,
}

impl Context {
    pub fn validation_counter(&self) -> ValidationCounter {
        self.validation.clone()
    }

    pub fn validation_enabled(&self) -> bool {
        self.messenger.is_some()
    }

    pub fn sync_validation_enabled(&self) -> bool {
        self.sync_validation
    }

    /// Surface-less context: NVIDIA discrete, else any discrete, else the first Vulkan 1.3 device
    /// with a graphics queue. With `validation` the layer must be installed; synchronization
    /// validation is enabled when the layer offers VK_EXT_validation_features.
    pub fn new_headless(validation: bool) -> Res<Self> {
        // SAFETY: no window handle is involved.
        unsafe { Self::create(None, validation).map(|(c, _)| c) }
    }

    /// Context whose device can present to `hwnd` (VK_KHR_swapchain enabled); returns the surface.
    ///
    /// # Safety
    /// `hwnd` must be a live window of the module `hinstance` and outlive the surface.
    pub unsafe fn with_window(hwnd: isize, hinstance: isize, validation: bool) -> Res<(Self, vk::SurfaceKHR)> {
        let (c, s) = Self::create(Some((hwnd, hinstance)), validation)?;
        Ok((c, s.expect("window context has a surface")))
    }

    unsafe fn create(window: Option<(isize, isize)>, validation: bool) -> Res<(Self, Option<vk::SurfaceKHR>)> {
        let entry = Entry::load()?;
        if validation && !layer_present(&entry) {
            return Err("validation needs VK_LAYER_KHRONOS_validation: install the Vulkan SDK".into());
        }
        let app = vk::ApplicationInfo::default()
            .application_name(c"imgui-spike")
            .api_version(vk::API_VERSION_1_3);
        let sync_validation =
            validation && window.is_none() && layer_has_extension(&entry, ext::validation_features::NAME);
        let counter = ValidationCounter::default();
        let mut exts = vec![khr::surface::NAME.as_ptr(), khr::win32_surface::NAME.as_ptr()];
        if validation {
            exts.push(ext::debug_utils::NAME.as_ptr());
        }
        if sync_validation {
            exts.push(ext::validation_features::NAME.as_ptr());
        }
        let layers = [VALIDATION_LAYER.as_ptr()];
        let mut instance_messenger = messenger_info(&counter);
        let enables = [vk::ValidationFeatureEnableEXT::SYNCHRONIZATION_VALIDATION];
        let mut features = vk::ValidationFeaturesEXT::default().enabled_validation_features(&enables);
        let mut info = vk::InstanceCreateInfo::default()
            .application_info(&app)
            .enabled_extension_names(&exts);
        if validation {
            info = info.enabled_layer_names(&layers).push_next(&mut instance_messenger);
        }
        if sync_validation {
            info = info.push_next(&mut features);
        }
        let instance = entry.create_instance(&info, None)?;
        let messenger = if validation {
            let du = ext::debug_utils::Instance::new(&entry, &instance);
            match du.create_debug_utils_messenger(&messenger_info(&counter), None) {
                Ok(m) => Some((du, m)),
                Err(e) => {
                    instance.destroy_instance(None);
                    return Err(e.into());
                }
            }
        } else {
            None
        };
        let surface_fn = khr::surface::Instance::new(&entry, &instance);
        let abandon = |surface: Option<vk::SurfaceKHR>| {
            if let Some(s) = surface {
                surface_fn.destroy_surface(s, None);
            }
            if let Some((du, m)) = &messenger {
                du.destroy_debug_utils_messenger(*m, None);
            }
            instance.destroy_instance(None);
        };
        let surface = match window {
            Some((hwnd, hinstance)) => {
                let win32 = khr::win32_surface::Instance::new(&entry, &instance);
                match win32.create_win32_surface(
                    &vk::Win32SurfaceCreateInfoKHR::default().hinstance(hinstance).hwnd(hwnd),
                    None,
                ) {
                    Ok(s) => Some(s),
                    Err(e) => {
                        abandon(None);
                        return Err(e.into());
                    }
                }
            }
            None => None,
        };
        match create_device(&instance, &surface_fn, surface) {
            Ok((physical, queue_family, device, gpu_name)) => {
                let queue = device.get_device_queue(queue_family, 0);
                let props = instance.get_physical_device_properties(physical);
                let qprops = instance.get_physical_device_queue_family_properties(physical);
                Ok((
                    Self {
                        entry,
                        instance,
                        physical,
                        device,
                        queue_family,
                        queue,
                        gpu_name,
                        timestamp_period_ns: props.limits.timestamp_period,
                        timestamp_valid_bits: qprops[queue_family as usize].timestamp_valid_bits,
                        messenger,
                        validation: counter,
                        sync_validation,
                    },
                    surface,
                ))
            }
            Err(e) => {
                abandon(surface);
                Err(e)
            }
        }
    }

    /// Memory type index matching `bits` with all of `flags`.
    pub fn memory_type(&self, bits: u32, flags: vk::MemoryPropertyFlags) -> Res<u32> {
        // SAFETY: `physical` belongs to the live `instance`.
        let props = unsafe { self.instance.get_physical_device_memory_properties(self.physical) };
        (0..props.memory_type_count)
            .find(|&i| bits & (1 << i) != 0 && props.memory_types[i as usize].property_flags.contains(flags))
            .ok_or_else(|| "no suitable memory type".into())
    }
}

/// # Safety
/// `surface`, if any, must be a live surface of `instance`.
unsafe fn create_device(
    instance: &Instance,
    surface_fn: &khr::surface::Instance,
    surface: Option<vk::SurfaceKHR>,
) -> Res<(vk::PhysicalDevice, u32, Device, String)> {
    let mut best: Option<(i32, vk::PhysicalDevice, u32, String)> = None;
    for pd in instance.enumerate_physical_devices()? {
        let p = instance.get_physical_device_properties(pd);
        if p.api_version < vk::API_VERSION_1_3 {
            continue;
        }
        let family = instance
            .get_physical_device_queue_family_properties(pd)
            .iter()
            .enumerate()
            .position(|(qi, q)| {
                q.queue_flags.contains(vk::QueueFlags::GRAPHICS)
                    && surface.is_none_or(|s| {
                        surface_fn
                            .get_physical_device_surface_support(pd, qi as u32, s)
                            .unwrap_or(false)
                    })
            });
        let Some(f) = family else { continue };
        let discrete = p.device_type == vk::PhysicalDeviceType::DISCRETE_GPU;
        let rank = i32::from(discrete) * 2 + i32::from(discrete && p.vendor_id == NVIDIA);
        let name = p
            .device_name_as_c_str()
            .map(|c| c.to_string_lossy().into_owned())
            .unwrap_or_default();
        if best.as_ref().is_none_or(|b| rank > b.0) {
            best = Some((rank, pd, f as u32, name));
        }
    }
    let (_, physical, queue_family, name) = best.ok_or("no usable Vulkan 1.3 device with a graphics queue")?;
    let priorities = [1.0f32];
    let queues = [vk::DeviceQueueCreateInfo::default()
        .queue_family_index(queue_family)
        .queue_priorities(&priorities)];
    let swapchain_ext = [khr::swapchain::NAME.as_ptr()];
    let dev_exts: &[*const std::ffi::c_char] = if surface.is_some() { &swapchain_ext } else { &[] };
    let mut f13 = vk::PhysicalDeviceVulkan13Features::default()
        .dynamic_rendering(true)
        .synchronization2(true);
    let device = instance.create_device(
        physical,
        &vk::DeviceCreateInfo::default()
            .queue_create_infos(&queues)
            .enabled_extension_names(dev_exts)
            .push_next(&mut f13),
        None,
    )?;
    Ok((physical, queue_family, device, name))
}

impl Drop for Context {
    fn drop(&mut self) {
        // SAFETY: owned handles destroyed once, device first, instance last; owners of child objects
        // destroyed them before (type doc).
        unsafe {
            let _ = self.device.device_wait_idle();
            self.device.destroy_device(None);
            if let Some((du, m)) = self.messenger.take() {
                du.destroy_debug_utils_messenger(m, None);
            }
            self.instance.destroy_instance(None);
        }
    }
}

pub fn color_range() -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange::default()
        .aspect_mask(vk::ImageAspectFlags::COLOR)
        .level_count(1)
        .layer_count(1)
}

/// Records an image layout barrier (synchronization2).
///
/// # Safety
/// `cb` must be recording on `d`; `image` must be a live image of `d`.
#[allow(clippy::too_many_arguments)]
pub unsafe fn barrier(
    d: &Device,
    cb: vk::CommandBuffer,
    image: vk::Image,
    old: vk::ImageLayout,
    new: vk::ImageLayout,
    src_stage: vk::PipelineStageFlags2,
    src_access: vk::AccessFlags2,
    dst_stage: vk::PipelineStageFlags2,
    dst_access: vk::AccessFlags2,
) {
    let b = [vk::ImageMemoryBarrier2::default()
        .src_stage_mask(src_stage)
        .src_access_mask(src_access)
        .dst_stage_mask(dst_stage)
        .dst_access_mask(dst_access)
        .old_layout(old)
        .new_layout(new)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(color_range())];
    d.cmd_pipeline_barrier2(cb, &vk::DependencyInfo::default().image_memory_barriers(&b));
}

/// Device-local 2D image with its colour view.
#[derive(Default)]
pub struct GpuImage {
    pub image: vk::Image,
    pub memory: vk::DeviceMemory,
    pub view: vk::ImageView,
}

impl GpuImage {
    /// # Safety
    /// `ctx` must be live; destroy the result with [`GpuImage::destroy`] once the GPU is done with it.
    pub unsafe fn create(
        ctx: &Context,
        extent: vk::Extent2D,
        format: vk::Format,
        usage: vk::ImageUsageFlags,
    ) -> Res<Self> {
        let d = &ctx.device;
        let mut g = Self {
            image: d.create_image(
                &vk::ImageCreateInfo::default()
                    .image_type(vk::ImageType::TYPE_2D)
                    .format(format)
                    .extent(vk::Extent3D {
                        width: extent.width,
                        height: extent.height,
                        depth: 1,
                    })
                    .mip_levels(1)
                    .array_layers(1)
                    .samples(vk::SampleCountFlags::TYPE_1)
                    .tiling(vk::ImageTiling::OPTIMAL)
                    .usage(usage)
                    .sharing_mode(vk::SharingMode::EXCLUSIVE)
                    .initial_layout(vk::ImageLayout::UNDEFINED),
                None,
            )?,
            ..Self::default()
        };
        let r = (|| -> Res<()> {
            let req = d.get_image_memory_requirements(g.image);
            g.memory = d.allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(req.size)
                    .memory_type_index(ctx.memory_type(req.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL)?),
                None,
            )?;
            d.bind_image_memory(g.image, g.memory, 0)?;
            g.view = d.create_image_view(
                &vk::ImageViewCreateInfo::default()
                    .image(g.image)
                    .view_type(vk::ImageViewType::TYPE_2D)
                    .format(format)
                    .subresource_range(color_range()),
                None,
            )?;
            Ok(())
        })();
        match r {
            Ok(()) => Ok(g),
            Err(e) => {
                g.destroy(d);
                Err(e)
            }
        }
    }

    /// # Safety
    /// The GPU must have finished every use of the image.
    pub unsafe fn destroy(&mut self, d: &Device) {
        d.destroy_image_view(self.view, None);
        d.destroy_image(self.image, None);
        d.free_memory(self.memory, None);
        *self = Self::default();
    }
}

/// Host-visible, host-coherent buffer, persistently mapped.
pub struct HostBuffer {
    pub buffer: vk::Buffer,
    pub memory: vk::DeviceMemory,
    pub mapped: *mut u8,
    pub size: usize,
}

impl HostBuffer {
    /// # Safety
    /// `ctx` must be live; destroy with [`HostBuffer::destroy`] once the GPU is done with it, and
    /// access `mapped` only within `size` bytes.
    pub unsafe fn create(ctx: &Context, size: usize, usage: vk::BufferUsageFlags) -> Res<Self> {
        let d = &ctx.device;
        let buffer = d.create_buffer(
            &vk::BufferCreateInfo::default()
                .size(size as u64)
                .usage(usage)
                .sharing_mode(vk::SharingMode::EXCLUSIVE),
            None,
        )?;
        let mut b = Self {
            buffer,
            memory: vk::DeviceMemory::null(),
            mapped: std::ptr::null_mut(),
            size,
        };
        let r = (|| -> Res<()> {
            let req = d.get_buffer_memory_requirements(b.buffer);
            let coherent = vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
            let index = ctx
                .memory_type(req.memory_type_bits, coherent | vk::MemoryPropertyFlags::HOST_CACHED)
                .or_else(|_| ctx.memory_type(req.memory_type_bits, coherent))?;
            b.memory = d.allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(req.size)
                    .memory_type_index(index),
                None,
            )?;
            d.bind_buffer_memory(b.buffer, b.memory, 0)?;
            b.mapped = d.map_memory(b.memory, 0, vk::WHOLE_SIZE, vk::MemoryMapFlags::empty())? as *mut u8;
            Ok(())
        })();
        match r {
            Ok(()) => Ok(b),
            Err(e) => {
                b.destroy(d);
                Err(e)
            }
        }
    }

    /// # Safety
    /// The GPU must have finished every use of the buffer; `mapped` is invalid afterwards.
    pub unsafe fn destroy(&mut self, d: &Device) {
        d.destroy_buffer(self.buffer, None);
        d.free_memory(self.memory, None);
        self.buffer = vk::Buffer::null();
        self.memory = vk::DeviceMemory::null();
        self.mapped = std::ptr::null_mut();
    }
}
