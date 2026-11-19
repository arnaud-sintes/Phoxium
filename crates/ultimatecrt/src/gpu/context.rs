//! Vulkan instance, physical device choice and logical device with its graphics queue.
//!
//! A [`Context`] is created either for a window ([`Context::with_window`], the device must be able
//! to present to it) or without any surface ([`Context::new_headless`], offline rendering and
//! tests). Device selection lives only here.

use std::ffi::{c_void, CStr};
use std::io::Write;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use ash::{ext, khr, vk, Device, Entry, Instance};

use super::Res;

const VALIDATION_LAYER: &CStr = c"VK_LAYER_KHRONOS_validation";

/// True when `UCRT_REQUIRE_GPU` is set to a non-empty value other than `0`: a missing GPU, validation
/// layer or synchronization validation then fails the GPU tests instead of skipping them.
pub fn gpu_required() -> bool {
    std::env::var("UCRT_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0")
}

/// True when the Vulkan loader is present and lists `VK_LAYER_KHRONOS_validation` (Vulkan SDK).
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

/// Shared count of the validation and performance warnings and errors reported by the validation
/// layer through the context's debug-utils messenger. Cloning shares the count, so a test can read
/// it after the context is dropped (messages raised while destroying the device count too).
#[derive(Clone, Default)]
pub struct ValidationCounter(Arc<AtomicU32>);

impl ValidationCounter {
    /// Messages counted so far.
    pub fn count(&self) -> u32 {
        self.0.load(Ordering::SeqCst)
    }
}

/// Debug-utils callback: prints every message to stderr and counts validation and performance
/// warnings and errors in the `AtomicU32` that `user` points to.
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
    // No panic may unwind into the loader: ignore a failed write.
    let _ = writeln!(std::io::stderr(), "vulkan {severity:?} {types:?}: {msg}");
    vk::FALSE
}

/// Messenger create info for warnings and errors of every type, counting into `counter`.
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

#[derive(Clone)]
pub struct GpuInfo {
    pub index: usize,
    pub name: String,
    pub kind: vk::PhysicalDeviceType,
    pub vendor_id: u32,
    pub api_version: u32,
    /// `VkPhysicalDeviceIDProperties::deviceLUID` (Windows LUID, little-endian bytes: low part then high
    /// part), `None` when the driver reports `deviceLUIDValid` false.
    pub luid: Option<[u8; 8]>,
}

const NVIDIA: u32 = 0x10DE;

/// Instance for the read-only queries (no layers, no extensions, no surface). Vulkan 1.3 like the renderer,
/// which also makes `vkGetPhysicalDeviceProperties2` available.
///
/// # Safety
/// `entry` is a loaded Vulkan entry; the caller destroys the instance.
unsafe fn query_instance(entry: &Entry) -> Res<Instance> {
    let app = vk::ApplicationInfo::default()
        .application_name(c"ultimatecrt")
        .api_version(vk::API_VERSION_1_3);
    Ok(entry.create_instance(&vk::InstanceCreateInfo::default().application_info(&app), None)?)
}

/// Lists the Vulkan devices without creating a surface.
pub fn list_gpus() -> Res<Vec<GpuInfo>> {
    let entry = unsafe { Entry::load()? };
    let instance = unsafe { query_instance(&entry)? };
    let out = unsafe { instance.enumerate_physical_devices() }.map(|devices| {
        devices
            .into_iter()
            .enumerate()
            .map(|(index, pd)| gpu_info(&instance, pd, index))
            .collect()
    });
    unsafe { instance.destroy_instance(None) };
    Ok(out?)
}

/// Whether a Vulkan device can import or export Direct3D 11 textures as external memory, from queries only
/// (T-03): no allocation, no image creation.
#[derive(Clone, Debug, Default)]
pub struct ExternalMemoryProbe {
    /// `VK_KHR_external_memory_win32` is offered by the device.
    pub win32_extension: bool,
    /// `VK_KHR_external_memory` (core in 1.1) or Vulkan 1.1 or later.
    pub base_extension: bool,
    /// `vkGetPhysicalDeviceImageFormatProperties2` for B8G8R8A8 with the D3D11 texture handle type:
    /// (importable, exportable), `None` when the query fails (type unsupported).
    pub d3d11_texture: Option<(bool, bool)>,
    /// The same for the D3D11 texture KMT handle type (legacy shared handle).
    pub d3d11_texture_kmt: Option<(bool, bool)>,
}

/// The Vulkan side of the adapter topology (T-03), see [`vulkan_report`].
pub struct VulkanReport {
    /// Every physical device, in enumeration order, with its external-memory probe.
    pub gpus: Vec<(GpuInfo, ExternalMemoryProbe)>,
    /// `GpuInfo::index` of the device the renderer picks today without `--gpu` (a window context).
    pub picked: Option<usize>,
}

/// Lists the Vulkan devices with LUID and external-memory support, and tells which one the renderer
/// picks by default today. Creates no surface, no logical device and no window.
pub fn vulkan_report() -> Res<VulkanReport> {
    let entry = unsafe { Entry::load()? };
    let instance = unsafe { query_instance(&entry)? };
    let out = (|| -> Res<VulkanReport> {
        let devices = unsafe { instance.enumerate_physical_devices()? };
        let gpus: Vec<_> = devices
            .iter()
            .enumerate()
            .map(|(i, &pd)| (gpu_info(&instance, pd, i), external_memory_probe(&instance, pd)))
            .collect();
        // Same candidates as `create_device` without a surface: Vulkan 1.3 and a graphics queue (a window
        // context additionally needs presentation support, which cannot be asked without a window).
        let usable: Vec<GpuInfo> = gpus
            .iter()
            .zip(&devices)
            .filter(|((g, _), &pd)| unsafe { usable_family(&instance, pd, g.api_version, None) }.is_some())
            .map(|((g, _), _)| g.clone())
            .collect();
        let picked = choose_device(&usable, None).map(|p| usable[p].index);
        Ok(VulkanReport { gpus, picked })
    })();
    unsafe { instance.destroy_instance(None) };
    out
}

fn external_memory_probe(instance: &Instance, pd: vk::PhysicalDevice) -> ExternalMemoryProbe {
    let exts = unsafe { instance.enumerate_device_extension_properties(pd) }.unwrap_or_default();
    let has = |name: &CStr| exts.iter().any(|e| e.extension_name_as_c_str() == Ok(name));
    let version = unsafe { instance.get_physical_device_properties(pd) }.api_version;
    let query = |handle: vk::ExternalMemoryHandleTypeFlags| -> Option<(bool, bool)> {
        let mut external = vk::PhysicalDeviceExternalImageFormatInfo::default().handle_type(handle);
        let info = vk::PhysicalDeviceImageFormatInfo2::default()
            .format(vk::Format::B8G8R8A8_UNORM)
            .ty(vk::ImageType::TYPE_2D)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::TRANSFER_DST)
            .push_next(&mut external);
        let mut ext_props = vk::ExternalImageFormatProperties::default();
        let mut props = vk::ImageFormatProperties2::default().push_next(&mut ext_props);
        // SAFETY: `pd` belongs to the live `instance` (1.3); the structs outlive the call.
        unsafe { instance.get_physical_device_image_format_properties2(pd, &info, &mut props) }.ok()?;
        let f = ext_props.external_memory_properties.external_memory_features;
        Some((
            f.contains(vk::ExternalMemoryFeatureFlags::IMPORTABLE),
            f.contains(vk::ExternalMemoryFeatureFlags::EXPORTABLE),
        ))
    };
    ExternalMemoryProbe {
        win32_extension: has(khr::external_memory_win32::NAME),
        base_extension: has(khr::external_memory::NAME) || version >= vk::API_VERSION_1_1,
        d3d11_texture: query(vk::ExternalMemoryHandleTypeFlags::D3D11_TEXTURE),
        d3d11_texture_kmt: query(vk::ExternalMemoryHandleTypeFlags::D3D11_TEXTURE_KMT),
    }
}

fn gpu_info(instance: &Instance, pd: vk::PhysicalDevice, index: usize) -> GpuInfo {
    let p = unsafe { instance.get_physical_device_properties(pd) };
    let mut id = vk::PhysicalDeviceIDProperties::default();
    let mut p2 = vk::PhysicalDeviceProperties2::default().push_next(&mut id);
    // SAFETY: `pd` belongs to `instance`, which is Vulkan 1.1 or later in every caller.
    unsafe { instance.get_physical_device_properties2(pd, &mut p2) };
    GpuInfo {
        luid: (id.device_luid_valid == vk::TRUE).then_some(id.device_luid),
        index,
        name: p
            .device_name_as_c_str()
            .map(|c| c.to_string_lossy().into_owned())
            .unwrap_or_default(),
        kind: p.device_type,
        vendor_id: p.vendor_id,
        api_version: p.api_version,
    }
}

/// Picks a device among the usable candidates: the requested `--gpu` index if usable, else NVIDIA
/// discrete, else any discrete, else the first. Returns a position in `candidates`.
fn choose_device(candidates: &[GpuInfo], requested: Option<usize>) -> Option<usize> {
    match requested {
        Some(i) => candidates.iter().position(|g| g.index == i),
        None => candidates
            .iter()
            .position(|g| g.vendor_id == NVIDIA && g.kind == vk::PhysicalDeviceType::DISCRETE_GPU)
            .or_else(|| {
                candidates
                    .iter()
                    .position(|g| g.kind == vk::PhysicalDeviceType::DISCRETE_GPU)
            })
            .or(if candidates.is_empty() { None } else { Some(0) }),
    }
}

/// Vulkan instance, chosen physical device, logical device and its graphics queue. Vulkan 1.3
/// with dynamic rendering and synchronization2. Owns the instance and the device and destroys them
/// in `Drop`: every object created from them (presenters, pipelines, textures) must be destroyed
/// first by its owner.
pub struct Context {
    pub entry: Entry,
    pub instance: Instance,
    pub surface_fn: khr::surface::Instance,
    pub physical: vk::PhysicalDevice,
    pub device: Device,
    pub queue_family: u32,
    pub queue: vk::Queue,
    pub gpu: GpuInfo,
    /// True when the device was created with `VK_KHR_swapchain` (window context).
    pub can_present: bool,
    /// Debug-utils messenger, present when validation is on. Destroyed in `Drop`, before the instance.
    messenger: Option<(ext::debug_utils::Instance, vk::DebugUtilsMessengerEXT)>,
    /// Count of validation messages; its `AtomicU32` is the messenger's user data, so it must
    /// outlive the instance (the field drops after `Drop::drop` destroyed the instance).
    validation: ValidationCounter,
    /// True when synchronization validation is on (`VK_EXT_validation_features` enabled).
    sync_validation: bool,
    /// True when the `syncval_shader_accesses_heuristic` layer setting was passed to the layer.
    shader_accesses: bool,
}

impl Context {
    /// Shared count of validation messages (always 0 when validation is off).
    pub fn validation_counter(&self) -> ValidationCounter {
        self.validation.clone()
    }

    /// True when the validation layer and the counting messenger are enabled.
    pub fn validation_enabled(&self) -> bool {
        self.messenger.is_some()
    }

    /// True when synchronization validation is active: needs the layer to offer
    /// VK_EXT_validation_features, else it is silently off. Without
    /// [`Context::shader_accesses_enabled`] it ignores the accesses made by shaders.
    pub fn sync_validation_enabled(&self) -> bool {
        self.sync_validation
    }

    /// True when the `syncval_shader_accesses_heuristic` layer setting was applied, so that
    /// synchronization validation also checks the accesses shaders make through descriptors. The
    /// setting is passed through VK_EXT_layer_settings, which the layer must offer (else it is not
    /// passed and this is false). The layer's settings are not enumerable through the Vulkan API, but
    /// a layer that does not know a setting reports it as a warning at `vkCreateInstance` (verified
    /// against SDK 1.4.363 with a made-up setting name): when that happens the instance is created
    /// again without the setting and this is false.
    pub fn shader_accesses_enabled(&self) -> bool {
        self.shader_accesses
    }

    /// Creates a surface-less context (no window, no swapchain extension): NVIDIA discrete, else
    /// any discrete, else the first Vulkan 1.3 device with a graphics queue.
    ///
    /// With `validation`, the Khronos validation layer must be installed (else an error), every
    /// message is printed to stderr and counted (see [`Context::validation_counter`]), and
    /// synchronization validation is enabled too when the layer offers `VK_EXT_validation_features`
    /// (headless contexts only: the window path has not been checked against it yet), including the
    /// accesses shaders make through descriptors when it offers `VK_EXT_layer_settings`.
    pub fn new_headless(validation: bool) -> Res<Self> {
        // SAFETY: no window handle is involved; `create` owns or destroys everything it creates.
        unsafe { Self::create(None, None, validation).map(|(ctx, _)| ctx) }
    }

    /// Creates a context whose device can present to the window `hwnd`, and returns the Vulkan
    /// surface created for it. The caller passes the surface to
    /// [`super::Presenter::from_surface`], which takes ownership of it.
    ///
    /// # Safety
    /// `hwnd` must be a live window of the module `hinstance` and stay alive until the returned
    /// surface is destroyed.
    pub unsafe fn with_window(
        hwnd: isize,
        hinstance: isize,
        gpu: Option<usize>,
        validation: bool,
    ) -> Res<(Self, vk::SurfaceKHR)> {
        let (ctx, surface) = Self::create(Some((hwnd, hinstance)), gpu, validation)?;
        Ok((ctx, surface.expect("window context has a surface")))
    }

    /// # Safety
    /// As [`Context::with_window`] when `window` is `Some`.
    unsafe fn create(
        window: Option<(isize, isize)>,
        gpu: Option<usize>,
        validation: bool,
    ) -> Res<(Self, Option<vk::SurfaceKHR>)> {
        let entry = Entry::load()?;
        if validation && !layer_present(&entry) {
            return Err("validation needs VK_LAYER_KHRONOS_validation: install the Vulkan SDK".into());
        }
        let app = vk::ApplicationInfo::default()
            .application_name(c"ultimatecrt")
            .api_version(vk::API_VERSION_1_3);
        let sync_validation =
            validation && window.is_none() && layer_has_extension(&entry, ext::validation_features::NAME);
        // Synchronization validation ignores the accesses that shaders make through descriptors unless
        // `syncval_shader_accesses_heuristic` is set (validation layer 1.4.3xx, off by default): without
        // it, a missing barrier between two shader stages is not reported (checked in T-20a).
        let mut shader_accesses = sync_validation && layer_has_extension(&entry, vk::EXT_LAYER_SETTINGS_NAME);
        let mut counter = ValidationCounter::default();
        let mut instance = create_instance(&entry, &app, validation, sync_validation, shader_accesses, &counter)?;
        if shader_accesses && counter.count() > 0 {
            // A layer that offers VK_EXT_layer_settings but predates the setting reports it as unknown
            // (a counted warning, checked against the installed layer): create the instance again
            // without it, and `shader_accesses_enabled` tells the callers.
            instance.destroy_instance(None);
            shader_accesses = false;
            counter = ValidationCounter::default();
            instance = create_instance(&entry, &app, validation, sync_validation, shader_accesses, &counter)?;
        }
        let surface_fn = khr::surface::Instance::new(&entry, &instance);

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
        // Destroys what was created so far, on an error path.
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
            Some((hwnd, hinstance)) => match create_win32_surface(&entry, &instance, hwnd, hinstance) {
                Ok(s) => Some(s),
                Err(e) => {
                    abandon(None);
                    return Err(e);
                }
            },
            None => None,
        };

        match create_device(&instance, &surface_fn, surface, gpu) {
            Ok((gpu, physical, queue_family, device)) => {
                let queue = device.get_device_queue(queue_family, 0);
                let ctx = Self {
                    entry,
                    instance,
                    surface_fn,
                    physical,
                    device,
                    queue_family,
                    queue,
                    gpu,
                    can_present: surface.is_some(),
                    messenger,
                    validation: counter,
                    sync_validation,
                    shader_accesses,
                };
                Ok((ctx, surface))
            }
            Err(e) => {
                abandon(surface);
                Err(e)
            }
        }
    }

    /// Memory type index matching `bits` (from memory requirements) with all of `flags`.
    pub fn memory_type(&self, bits: u32, flags: vk::MemoryPropertyFlags) -> Res<u32> {
        // SAFETY: `physical` belongs to the live `instance`.
        let props = unsafe { self.instance.get_physical_device_memory_properties(self.physical) };
        (0..props.memory_type_count)
            .find(|&i| bits & (1 << i) != 0 && props.memory_types[i as usize].property_flags.contains(flags))
            .ok_or_else(|| "no suitable memory type".into())
    }
}

/// Creates the instance with the surface extensions and, as asked, the validation layer with its
/// counting messenger, synchronization validation and the shader-access heuristic setting.
///
/// # Safety
/// `entry` is a loaded Vulkan entry; the extensions asked for are offered by the layer.
unsafe fn create_instance(
    entry: &Entry,
    app: &vk::ApplicationInfo<'_>,
    validation: bool,
    sync_validation: bool,
    shader_accesses: bool,
    counter: &ValidationCounter,
) -> Res<Instance> {
    // Surface extensions are always enabled so that presenters can be added for new windows.
    let mut exts = vec![khr::surface::NAME.as_ptr(), khr::win32_surface::NAME.as_ptr()];
    if validation {
        exts.push(ext::debug_utils::NAME.as_ptr());
    }
    if sync_validation {
        exts.push(ext::validation_features::NAME.as_ptr());
    }
    if shader_accesses {
        exts.push(vk::EXT_LAYER_SETTINGS_NAME.as_ptr());
    }
    let heuristic_on: vk::Bool32 = vk::TRUE;
    let mut heuristic = vk::LayerSettingEXT::default()
        .layer_name(VALIDATION_LAYER)
        .setting_name(c"syncval_shader_accesses_heuristic")
        .ty(vk::LayerSettingTypeEXT::BOOL32);
    // One VkBool32 value (the `values` builder would count bytes, not values).
    heuristic.value_count = 1;
    heuristic.p_values = (&heuristic_on as *const vk::Bool32).cast();
    let layer_settings = [heuristic];
    let mut settings = vk::LayerSettingsCreateInfoEXT::default().settings(&layer_settings);
    let layers = [VALIDATION_LAYER.as_ptr()];
    // Chained into the instance create info, this messenger also reports messages raised by
    // vkCreateInstance and vkDestroyInstance.
    let mut instance_messenger = messenger_info(counter);
    let enables = [vk::ValidationFeatureEnableEXT::SYNCHRONIZATION_VALIDATION];
    let mut features = vk::ValidationFeaturesEXT::default().enabled_validation_features(&enables);
    let mut info = vk::InstanceCreateInfo::default()
        .application_info(app)
        .enabled_extension_names(&exts);
    if validation {
        info = info.enabled_layer_names(&layers).push_next(&mut instance_messenger);
    }
    if sync_validation {
        info = info.push_next(&mut features);
    }
    if shader_accesses {
        info = info.push_next(&mut settings);
    }
    Ok(entry.create_instance(&info, None)?)
}
/// Creates a Win32 Vulkan surface.
///
/// # Safety
/// `instance` must have the surface extensions enabled; `hwnd` must be a live window of `hinstance`.
pub(super) unsafe fn create_win32_surface(
    entry: &Entry,
    instance: &Instance,
    hwnd: isize,
    hinstance: isize,
) -> Res<vk::SurfaceKHR> {
    let win32 = khr::win32_surface::Instance::new(entry, instance);
    Ok(win32.create_win32_surface(
        &vk::Win32SurfaceCreateInfoKHR::default().hinstance(hinstance).hwnd(hwnd),
        None,
    )?)
}

/// Graphics queue family of a candidate device, or `None` when the device is not a candidate: it must be
/// Vulkan 1.3 and, with a surface, the family must present to it.
///
/// # Safety
/// `pd` belongs to `instance`; the surface, if any, is live and made by that instance.
unsafe fn usable_family(
    instance: &Instance,
    pd: vk::PhysicalDevice,
    api_version: u32,
    surface: Option<(&khr::surface::Instance, vk::SurfaceKHR)>,
) -> Option<u32> {
    if api_version < vk::API_VERSION_1_3 {
        return None;
    }
    instance
        .get_physical_device_queue_family_properties(pd)
        .iter()
        .enumerate()
        .position(|(qi, q)| {
            q.queue_flags.contains(vk::QueueFlags::GRAPHICS)
                && surface.is_none_or(|(surface_fn, s)| {
                    surface_fn
                        .get_physical_device_surface_support(pd, qi as u32, s)
                        .unwrap_or(false)
                })
        })
        .map(|f| f as u32)
}

/// Chooses the physical device and creates the logical device. With a surface, only devices
/// with a queue family that does graphics and presents to it are candidates, and the device gets
/// `VK_KHR_swapchain`.
///
/// # Safety
/// `surface`, if any, must be a live surface of `instance`.
unsafe fn create_device(
    instance: &Instance,
    surface_fn: &khr::surface::Instance,
    surface: Option<vk::SurfaceKHR>,
    gpu: Option<usize>,
) -> Res<(GpuInfo, vk::PhysicalDevice, u32, Device)> {
    let mut infos = Vec::new();
    let mut handles = Vec::new();
    for (i, pd) in instance.enumerate_physical_devices()?.into_iter().enumerate() {
        let g = gpu_info(instance, pd, i);
        if let Some(f) = usable_family(instance, pd, g.api_version, surface.map(|s| (surface_fn, s))) {
            infos.push(g);
            handles.push((pd, f));
        }
    }
    let chosen = choose_device(&infos, gpu).ok_or(if surface.is_some() {
        "no Vulkan 1.3 device can present to the output window (see --list-gpus)"
    } else {
        "no usable Vulkan 1.3 device with a graphics queue (see --list-gpus)"
    })?;
    let (physical, queue_family) = handles[chosen];
    let g = infos.swap_remove(chosen);

    let priorities = [1.0f32];
    let queues = [vk::DeviceQueueCreateInfo::default()
        .queue_family_index(queue_family)
        .queue_priorities(&priorities)];
    let swapchain_ext = [khr::swapchain::NAME.as_ptr()];
    let dev_exts: &[*const std::ffi::c_char] = if surface.is_some() { &swapchain_ext } else { &[] };
    let mut f13 = vk::PhysicalDeviceVulkan13Features::default()
        .dynamic_rendering(true)
        .synchronization2(true);
    // `encode` counts clipped pixels with atomics in a storage buffer (T-46); universal on desktop GPUs.
    let features = vk::PhysicalDeviceFeatures::default().fragment_stores_and_atomics(true);
    let device = instance.create_device(
        physical,
        &vk::DeviceCreateInfo::default()
            .queue_create_infos(&queues)
            .enabled_extension_names(dev_exts)
            .enabled_features(&features)
            .push_next(&mut f13),
        None,
    )?;
    Ok((g, physical, queue_family, device))
}

impl Drop for Context {
    fn drop(&mut self) {
        // SAFETY: the device, messenger and instance are owned by self and destroyed once, device
        // first, instance last; the owners of child objects destroy them before dropping the
        // context (see the type doc). The counter the messenger writes to drops after this body.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn info(index: usize, vendor_id: u32, kind: vk::PhysicalDeviceType) -> GpuInfo {
        GpuInfo {
            index,
            name: format!("gpu{index}"),
            kind,
            vendor_id,
            api_version: vk::API_VERSION_1_3,
            luid: None,
        }
    }

    #[test]
    fn choice_prefers_nvidia_discrete_then_discrete_then_first() {
        let intel = info(0, 0x8086, vk::PhysicalDeviceType::INTEGRATED_GPU);
        let amd = info(1, 0x1002, vk::PhysicalDeviceType::DISCRETE_GPU);
        let nv = info(2, NVIDIA, vk::PhysicalDeviceType::DISCRETE_GPU);
        assert_eq!(choose_device(&[intel, amd, nv], None), Some(2));
        let intel = info(0, 0x8086, vk::PhysicalDeviceType::INTEGRATED_GPU);
        let amd = info(1, 0x1002, vk::PhysicalDeviceType::DISCRETE_GPU);
        assert_eq!(choose_device(&[intel, amd], None), Some(1));
        let intel = info(0, 0x8086, vk::PhysicalDeviceType::INTEGRATED_GPU);
        assert_eq!(choose_device(&[intel], None), Some(0));
        assert_eq!(choose_device(&[], None), None);
    }

    #[test]
    fn choice_honours_requested_index() {
        let intel = info(0, 0x8086, vk::PhysicalDeviceType::INTEGRATED_GPU);
        let nv = info(3, NVIDIA, vk::PhysicalDeviceType::DISCRETE_GPU);
        // The requested value is the Vulkan enumeration index, not the candidate position.
        assert_eq!(choose_device(&[intel, nv], Some(0)), Some(0));
        let nv = info(3, NVIDIA, vk::PhysicalDeviceType::DISCRETE_GPU);
        assert_eq!(choose_device(&[nv], Some(1)), None);
    }

    /// The messenger counts validation and performance warnings and errors, not general ones.
    /// Needs the validation layer and a Vulkan 1.3 device; skipped without them unless
    /// `UCRT_REQUIRE_GPU=1`.
    #[test]
    fn messenger_counts_validation_messages() {
        let require = gpu_required();
        if !validation_layer_available() {
            assert!(!require, "UCRT_REQUIRE_GPU=1 but the validation layer is not installed");
            eprintln!("messenger_counts_validation_messages skipped: no validation layer");
            return;
        }
        let ctx = match Context::new_headless(true) {
            Ok(ctx) => ctx,
            Err(e) => {
                assert!(!require, "UCRT_REQUIRE_GPU=1 but no headless Vulkan device: {e}");
                eprintln!("messenger_counts_validation_messages skipped: {e}");
                return;
            }
        };
        assert!(ctx.validation_enabled());
        let counter = ctx.validation_counter();
        let (du, _) = ctx.messenger.as_ref().expect("messenger");
        let send = |types: vk::DebugUtilsMessageTypeFlagsEXT| {
            let data = vk::DebugUtilsMessengerCallbackDataEXT::default()
                .message(c"ultimatecrt messenger self-test (expected, ignore)");
            // SAFETY: the instance has VK_EXT_debug_utils enabled (validation on) and is live.
            unsafe { du.submit_debug_utils_message(vk::DebugUtilsMessageSeverityFlagsEXT::ERROR, types, &data) };
        };
        send(vk::DebugUtilsMessageTypeFlagsEXT::GENERAL);
        assert_eq!(counter.count(), 0);
        send(vk::DebugUtilsMessageTypeFlagsEXT::VALIDATION);
        send(vk::DebugUtilsMessageTypeFlagsEXT::PERFORMANCE);
        assert_eq!(counter.count(), 2);
        drop(ctx);
        assert_eq!(counter.count(), 2, "destroying a clean context reports nothing");
    }

    /// Needs a Vulkan 1.3 driver; no window is created. Skips without one unless
    /// `UCRT_REQUIRE_GPU=1` (same rule as `tests/common/mod.rs`).
    #[test]
    fn headless_device() {
        let ctx = match Context::new_headless(false) {
            Ok(ctx) => ctx,
            Err(e) if !gpu_required() => {
                eprintln!("headless_device skipped: no Vulkan 1.3 device ({e})");
                return;
            }
            Err(e) => panic!("UCRT_REQUIRE_GPU=1 but no headless Vulkan device: {e}"),
        };
        assert!(!ctx.gpu.name.is_empty());
        assert!(!ctx.can_present);
        assert!(!ctx.validation_enabled());
    }
}
