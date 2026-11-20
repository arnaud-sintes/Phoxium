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

use super::{GpuError, Res};
use crate::quality::ImageLimits;
use crate::zerocopy::{select_handle, HandleKind};

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
    /// `GpuInfo::index` of the candidates (Vulkan 1.3, a graphics and compute queue, the features and limits of
    /// [`device_meets`]), in enumeration order.
    pub usable: Vec<usize>,
    /// `GpuInfo::index` of the device [`DeviceRequest::Default`] picks (a window context).
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
        // Same candidates as `create_device` without a surface: Vulkan 1.3, a graphics and compute queue, the
        // features and limits of `device_meets` (a window context additionally needs presentation support, which
        // cannot be asked without a window).
        let usable: Vec<GpuInfo> = gpus
            .iter()
            .zip(&devices)
            .filter(|((g, _), &pd)| unsafe { candidate(&instance, pd, g.api_version, None) }.is_ok())
            .map(|((g, _), _)| g.clone())
            .collect();
        let picked = choose_device(&usable, DeviceRequest::Default).map(|p| usable[p].index);
        Ok(VulkanReport {
            gpus,
            usable: usable.iter().map(|g| g.index).collect(),
            picked,
        })
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

/// Which Vulkan device the renderer should run on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DeviceRequest {
    /// NVIDIA discrete, else any discrete, else the first usable device (the choice before `gpu.adapter`).
    #[default]
    Default,
    /// A Vulkan enumeration index (`--gpu`, `--list-gpus`).
    Index(usize),
    /// A Windows adapter LUID, matched against `VkPhysicalDeviceIDProperties::deviceLUID` (bytes as Vulkan
    /// reports them, see `adapters::Luid::to_vulkan`).
    Luid([u8; 8]),
}

/// Picks a device among the usable candidates: the requested index or LUID if usable, else (no request)
/// NVIDIA discrete, else any discrete, else the first. Returns a position in `candidates`.
fn choose_device(candidates: &[GpuInfo], requested: DeviceRequest) -> Option<usize> {
    match requested {
        DeviceRequest::Index(i) => candidates.iter().position(|g| g.index == i),
        DeviceRequest::Luid(l) => candidates.iter().position(|g| g.luid == Some(l)),
        DeviceRequest::Default => candidates
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
    /// What one chain image may be on this device (R-02): `maxImageDimension2D` and the memory budget.
    image_limits: ImageLimits,
    /// The zero-copy capture path was asked for when the device was created (A29).
    external_requested: bool,
    /// `VK_KHR_external_memory_win32` is enabled, with the handle kind of the shared Direct3D 11 textures the device
    /// imports (`zerocopy::select_handle`); `None` when it was not asked for or the device cannot import.
    external_memory: Option<HandleKind>,
    /// The extension's functions, with `external_memory`.
    pub(super) external_fn: Option<khr::external_memory_win32::Device>,
}

impl Context {
    /// The largest image side (`maxImageDimension2D`) and the memory budget of one chain image on this device
    /// ([`crate::quality::image_budget`] of its largest device-local heap): the pipeline limits the amplifier K and
    /// the other image sizes to them (R-02).
    pub fn image_limits(&self) -> ImageLimits {
        self.image_limits
    }

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
    /// any discrete, else the first supported device ([`device_meets`]: Vulkan 1.3, a graphics and compute
    /// queue, the features and compute limits the shaders need).
    ///
    /// With `validation`, the Khronos validation layer must be installed (else an error), every
    /// message is printed to stderr and counted (see [`Context::validation_counter`]), and
    /// synchronization validation is enabled too when the layer offers `VK_EXT_validation_features`
    /// (headless contexts only: the window path has not been checked against it yet), including the
    /// accesses shaders make through descriptors when it offers `VK_EXT_layer_settings`.
    pub fn new_headless(validation: bool) -> Res<Self> {
        // SAFETY: no window handle is involved; `create` owns or destroys everything it creates.
        unsafe { Self::create(None, DeviceRequest::Default, validation, false).map(|(ctx, _)| ctx) }
    }

    /// [`Context::new_headless`] with `VK_KHR_external_memory_win32` enabled when the device offers it and imports
    /// Direct3D 11 textures ([`Context::external_memory`]): the zero-copy tests and measurements.
    pub fn new_headless_external(validation: bool) -> Res<Self> {
        // SAFETY: as in `new_headless`.
        unsafe { Self::create(None, DeviceRequest::Default, validation, true).map(|(ctx, _)| ctx) }
    }

    /// The handle kind of the shared Direct3D 11 textures this device imports, when `VK_KHR_external_memory_win32`
    /// was asked for at creation and enabled (zero-copy capture, A29); `None` otherwise.
    pub fn external_memory(&self) -> Option<HandleKind> {
        self.external_memory
    }

    /// Whether the physical device offers `VK_KHR_external_memory_win32` (a query, whatever was enabled).
    pub fn external_memory_offered(&self) -> bool {
        // SAFETY: `physical` belongs to the live `instance`.
        unsafe { self.instance.enumerate_device_extension_properties(self.physical) }
            .unwrap_or_default()
            .iter()
            .any(|e| e.extension_name_as_c_str() == Ok(khr::external_memory_win32::NAME))
    }

    /// Whether the device was created for the zero-copy path (the extension asked for, whether or not offered).
    pub fn external_memory_requested(&self) -> bool {
        self.external_requested
    }

    /// Creates a context whose device can present to the window `hwnd`, and returns the Vulkan
    /// surface created for it. With `external_memory` (the application always asks, so that the zero-copy capture
    /// path can be switched on at any time, A29), `VK_KHR_external_memory_win32` is enabled when the device offers
    /// it and imports Direct3D 11 textures. The caller passes the surface to
    /// [`super::Presenter::from_surface`], which takes ownership of it.
    ///
    /// # Safety
    /// `hwnd` must be a live window of the module `hinstance` and stay alive until the returned
    /// surface is destroyed.
    pub unsafe fn with_window(
        hwnd: isize,
        hinstance: isize,
        gpu: DeviceRequest,
        validation: bool,
        external_memory: bool,
    ) -> Res<(Self, vk::SurfaceKHR)> {
        let (ctx, surface) = Self::create(Some((hwnd, hinstance)), gpu, validation, external_memory)?;
        Ok((ctx, surface.expect("window context has a surface")))
    }

    /// # Safety
    /// As [`Context::with_window`] when `window` is `Some`.
    unsafe fn create(
        window: Option<(isize, isize)>,
        gpu: DeviceRequest,
        validation: bool,
        external: bool,
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

        match create_device(&instance, &surface_fn, surface, gpu, external) {
            Ok((gpu, physical, queue_family, device, external_memory)) => {
                let external_fn = external_memory.map(|_| khr::external_memory_win32::Device::new(&instance, &device));
                let queue = device.get_device_queue(queue_family, 0);
                let image_limits = image_limits_of(&instance, physical);
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
                    image_limits,
                    external_requested: external,
                    external_memory,
                    external_fn,
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

/// Workgroup invocations the shaders use at most (`reduce*.comp`: 256 x 1; `screen.comp`: the 8 x 32 beam tile).
/// The Vulkan minimum is 128.
pub const NEEDED_INVOCATIONS: u32 = 256;
/// Workgroup size per axis the shaders use at most (`reduce*.comp`: x 256; `screen.comp`: y 32). The Vulkan
/// minimum is (128, 128, 64).
pub const NEEDED_GROUP_SIZE: [u32; 3] = [256, 32, 1];
/// Shared memory per workgroup the shaders use at most, bytes: `composite.comp`, 7 arrays of 1024 floats (28 KiB);
/// the beam tile of `beam.glsl` takes 5 x 256 `vec3` (20 KiB with 16-byte padding), `optics_kernel.comp` 9.4 KiB.
/// The Vulkan minimum is 16384 (a test checks these figures against the shader sources).
pub const NEEDED_SHARED_MEMORY: u32 = 7 * 1024 * 4;

/// What device selection checks on a device (R-29), from queries only ([`device_meets`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceCaps {
    pub api_version: u32,
    /// A queue family does graphics and compute (and presents to the window when one is asked).
    pub queue_family: bool,
    /// A window is asked, so the device needs `VK_KHR_swapchain`.
    pub needs_swapchain: bool,
    /// The device offers `VK_KHR_swapchain`.
    pub swapchain: bool,
    pub fragment_stores_and_atomics: bool,
    pub dynamic_rendering: bool,
    pub synchronization2: bool,
    pub max_compute_shared_memory_size: u32,
    pub max_compute_work_group_invocations: u32,
    pub max_compute_work_group_size: [u32; 3],
}

/// Whether a device can run the renderer (R-29): Vulkan 1.3; a queue family with graphics and compute (that
/// presents, with a window); `VK_KHR_swapchain` with a window; the features `dynamicRendering`, `synchronization2`
/// and `fragmentStoresAndAtomics` (`encode`'s clip counters); and the compute limits the shaders need
/// ([`NEEDED_INVOCATIONS`], [`NEEDED_GROUP_SIZE`], [`NEEDED_SHARED_MEMORY`]). The error names what is missing.
pub fn device_meets(c: &DeviceCaps) -> Result<(), String> {
    if c.api_version < vk::API_VERSION_1_3 {
        return Err(format!(
            "Vulkan {}.{} only, Vulkan 1.3 is needed (update the graphics driver)",
            vk::api_version_major(c.api_version),
            vk::api_version_minor(c.api_version)
        ));
    }
    if !c.queue_family {
        return Err(if c.needs_swapchain {
            "no queue family does graphics and compute and presents to the output window".into()
        } else {
            "no queue family does graphics and compute".into()
        });
    }
    if c.needs_swapchain && !c.swapchain {
        return Err("the device does not offer VK_KHR_swapchain".into());
    }
    for (have, name) in [
        (c.dynamic_rendering, "dynamicRendering"),
        (c.synchronization2, "synchronization2"),
        (c.fragment_stores_and_atomics, "fragmentStoresAndAtomics"),
    ] {
        if !have {
            return Err(format!("the feature {name} is not supported"));
        }
    }
    if c.max_compute_work_group_invocations < NEEDED_INVOCATIONS {
        return Err(format!(
            "maxComputeWorkGroupInvocations is {}, the shaders need {NEEDED_INVOCATIONS}",
            c.max_compute_work_group_invocations
        ));
    }
    for (axis, (&have, &need)) in ["x", "y", "z"]
        .iter()
        .zip(c.max_compute_work_group_size.iter().zip(&NEEDED_GROUP_SIZE))
    {
        if have < need {
            return Err(format!(
                "maxComputeWorkGroupSize.{axis} is {have}, the shaders need {need}"
            ));
        }
    }
    if c.max_compute_shared_memory_size < NEEDED_SHARED_MEMORY {
        return Err(format!(
            "maxComputeSharedMemorySize is {} bytes, the shaders need {NEEDED_SHARED_MEMORY}",
            c.max_compute_shared_memory_size
        ));
    }
    Ok(())
}

/// The queue family of a candidate device, or why the device is not one ([`device_meets`]). With a surface, the
/// family must also present to it.
///
/// # Safety
/// `pd` belongs to `instance` (Vulkan 1.3); the surface, if any, is live and made by that instance.
unsafe fn candidate(
    instance: &Instance,
    pd: vk::PhysicalDevice,
    api_version: u32,
    surface: Option<(&khr::surface::Instance, vk::SurfaceKHR)>,
) -> Result<u32, String> {
    let mut caps = DeviceCaps {
        api_version,
        queue_family: false,
        needs_swapchain: surface.is_some(),
        swapchain: false,
        fragment_stores_and_atomics: false,
        dynamic_rendering: false,
        synchronization2: false,
        max_compute_shared_memory_size: 0,
        max_compute_work_group_invocations: 0,
        max_compute_work_group_size: [0; 3],
    };
    if api_version < vk::API_VERSION_1_3 {
        return device_meets(&caps).map(|()| 0);
    }
    let family = instance
        .get_physical_device_queue_family_properties(pd)
        .iter()
        .enumerate()
        .position(|(qi, q)| {
            q.queue_flags
                .contains(vk::QueueFlags::GRAPHICS | vk::QueueFlags::COMPUTE)
                && surface.is_none_or(|(surface_fn, s)| {
                    surface_fn
                        .get_physical_device_surface_support(pd, qi as u32, s)
                        .unwrap_or(false)
                })
        });
    caps.queue_family = family.is_some();
    caps.swapchain = instance
        .enumerate_device_extension_properties(pd)
        .unwrap_or_default()
        .iter()
        .any(|e| e.extension_name_as_c_str() == Ok(khr::swapchain::NAME));
    let mut f13 = vk::PhysicalDeviceVulkan13Features::default();
    let mut f2 = vk::PhysicalDeviceFeatures2::default().push_next(&mut f13);
    instance.get_physical_device_features2(pd, &mut f2);
    caps.fragment_stores_and_atomics = f2.features.fragment_stores_and_atomics == vk::TRUE;
    caps.dynamic_rendering = f13.dynamic_rendering == vk::TRUE;
    caps.synchronization2 = f13.synchronization2 == vk::TRUE;
    let l = instance.get_physical_device_properties(pd).limits;
    caps.max_compute_shared_memory_size = l.max_compute_shared_memory_size;
    caps.max_compute_work_group_invocations = l.max_compute_work_group_invocations;
    caps.max_compute_work_group_size = l.max_compute_work_group_size;
    device_meets(&caps)?;
    Ok(family.unwrap_or_default() as u32)
}

/// The image limits of a device ([`Context::image_limits`]).
///
/// # Safety
/// `pd` belongs to the live `instance`.
unsafe fn image_limits_of(instance: &Instance, pd: vk::PhysicalDevice) -> ImageLimits {
    let max_dim = instance
        .get_physical_device_properties(pd)
        .limits
        .max_image_dimension2_d;
    let mem = instance.get_physical_device_memory_properties(pd);
    let heap = mem.memory_heaps[..mem.memory_heap_count as usize]
        .iter()
        .filter(|h| h.flags.contains(vk::MemoryHeapFlags::DEVICE_LOCAL))
        .map(|h| h.size)
        .max()
        .unwrap_or(0);
    ImageLimits {
        max_dim,
        budget_bytes: crate::quality::image_budget(heap),
    }
}

/// Chooses the physical device and creates the logical device. Only devices that meet
/// [`device_meets`] are candidates (with a surface: a queue family that does graphics and compute and
/// presents to it, and `VK_KHR_swapchain`, which the device then gets). A requested device that is not
/// supported is reported with the reason and the default choice is used; with no candidate at all the
/// error ([`GpuError::Unsupported`]) names every device and why.
///
/// # Safety
/// `surface`, if any, must be a live surface of `instance`.
unsafe fn create_device(
    instance: &Instance,
    surface_fn: &khr::surface::Instance,
    surface: Option<vk::SurfaceKHR>,
    gpu: DeviceRequest,
    external: bool,
) -> Res<(GpuInfo, vk::PhysicalDevice, u32, Device, Option<HandleKind>)> {
    let mut infos = Vec::new();
    let mut handles = Vec::new();
    let mut rejected: Vec<(GpuInfo, String)> = Vec::new();
    for (i, pd) in instance.enumerate_physical_devices()?.into_iter().enumerate() {
        let g = gpu_info(instance, pd, i);
        match candidate(instance, pd, g.api_version, surface.map(|s| (surface_fn, s))) {
            Ok(f) => {
                infos.push(g);
                handles.push((pd, f));
            }
            Err(reason) => rejected.push((g, reason)),
        }
    }
    // A LUID comes from the adapter selection (`adapters::select`); when that device cannot present to the
    // window, is not supported (or vanished), the default choice is used and the caller sees it in `Context::gpu`.
    let chosen = choose_device(&infos, gpu).or_else(|| {
        let why = |g: &GpuInfo| {
            rejected
                .iter()
                .find(|(r, _)| r.index == g.index)
                .map(|(r, reason)| format!("{} is not supported ({reason})", r.name))
        };
        match gpu {
            DeviceRequest::Luid(l) => {
                let reason = rejected
                    .iter()
                    .find(|(r, _)| r.luid == Some(l))
                    .and_then(|(r, _)| why(r))
                    .unwrap_or_else(|| {
                        format!(
                            "no usable Vulkan device with LUID {}{}",
                            crate::adapters::Luid::from_vulkan(l),
                            if surface.is_some() {
                                " can present to the output window"
                            } else {
                                ""
                            }
                        )
                    });
                eprintln!("warning: {reason}; using the default device");
                choose_device(&infos, DeviceRequest::Default)
            }
            DeviceRequest::Index(i) => {
                // An existing but unsupported device falls back with a warning; an unknown index stays an error.
                let reason = rejected.iter().find(|(r, _)| r.index == i).and_then(|(r, _)| why(r))?;
                eprintln!("warning: {reason}; using the default device");
                choose_device(&infos, DeviceRequest::Default)
            }
            DeviceRequest::Default => None,
        }
    });
    let chosen = match chosen {
        Some(c) => c,
        None if !rejected.is_empty() && infos.is_empty() => {
            let list: Vec<String> = rejected.iter().map(|(g, r)| format!("{}: {r}", g.name)).collect();
            return Err(Box::new(GpuError::Unsupported {
                reason: format!("{} (see --list-gpus)", list.join("; ")),
            }));
        }
        None => {
            return Err(if surface.is_some() {
                "no Vulkan 1.3 device can present to the output window (see --list-gpus)".into()
            } else {
                "no usable Vulkan 1.3 device with a graphics queue (see --list-gpus)".into()
            })
        }
    };
    let (physical, queue_family) = handles[chosen];
    let g = infos.swap_remove(chosen);

    let priorities = [1.0f32];
    let queues = [vk::DeviceQueueCreateInfo::default()
        .queue_family_index(queue_family)
        .queue_priorities(&priorities)];
    let mut dev_exts: Vec<*const std::ffi::c_char> = Vec::new();
    if surface.is_some() {
        dev_exts.push(khr::swapchain::NAME.as_ptr());
    }
    // Zero-copy capture (A29): the extension when asked (always for the application's window device, so the path can
    // be switched on during a session), offered, and the device imports a Direct3D 11 texture handle in B8G8R8A8
    // (`VK_KHR_external_memory`, its dependency, and dedicated allocations are core 1.1). Nothing else changes on
    // the device: the CPU path never touches the extension.
    let external_memory = if external {
        let probe = external_memory_probe(instance, physical);
        let kind = select_handle(probe.d3d11_texture, probe.d3d11_texture_kmt).filter(|_| probe.win32_extension);
        if kind.is_some() {
            dev_exts.push(khr::external_memory_win32::NAME.as_ptr());
        }
        kind
    } else {
        None
    };
    let mut f13 = vk::PhysicalDeviceVulkan13Features::default()
        .dynamic_rendering(true)
        .synchronization2(true);
    // `encode` counts clipped pixels with atomics in a storage buffer (T-46); every candidate supports it
    // (`device_meets`, R-29), as it does dynamic rendering and synchronization2.
    let features = vk::PhysicalDeviceFeatures::default().fragment_stores_and_atomics(true);
    let device = instance.create_device(
        physical,
        &vk::DeviceCreateInfo::default()
            .queue_create_infos(&queues)
            .enabled_extension_names(&dev_exts)
            .enabled_features(&features)
            .push_next(&mut f13),
        None,
    )?;
    Ok((g, physical, queue_family, device, external_memory))
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
        let d = DeviceRequest::Default;
        assert_eq!(choose_device(&[intel, amd, nv], d), Some(2));
        let intel = info(0, 0x8086, vk::PhysicalDeviceType::INTEGRATED_GPU);
        let amd = info(1, 0x1002, vk::PhysicalDeviceType::DISCRETE_GPU);
        assert_eq!(choose_device(&[intel, amd], d), Some(1));
        let intel = info(0, 0x8086, vk::PhysicalDeviceType::INTEGRATED_GPU);
        assert_eq!(choose_device(&[intel], d), Some(0));
        assert_eq!(choose_device(&[], d), None);
    }

    #[test]
    fn choice_honours_requested_index() {
        let intel = info(0, 0x8086, vk::PhysicalDeviceType::INTEGRATED_GPU);
        let nv = info(3, NVIDIA, vk::PhysicalDeviceType::DISCRETE_GPU);
        // The requested value is the Vulkan enumeration index, not the candidate position.
        assert_eq!(choose_device(&[intel, nv], DeviceRequest::Index(0)), Some(0));
        let nv = info(3, NVIDIA, vk::PhysicalDeviceType::DISCRETE_GPU);
        assert_eq!(choose_device(&[nv], DeviceRequest::Index(1)), None);
    }

    #[test]
    fn choice_by_luid_takes_the_matching_device_even_if_integrated() {
        let mut intel = info(0, 0x8086, vk::PhysicalDeviceType::INTEGRATED_GPU);
        intel.luid = Some([0x26, 0x74, 1, 0, 0, 0, 0, 0]);
        let mut nv = info(1, NVIDIA, vk::PhysicalDeviceType::DISCRETE_GPU);
        nv.luid = Some([1, 0x78, 1, 0, 0, 0, 0, 0]);
        let c = [intel, nv];
        assert_eq!(
            choose_device(&c, DeviceRequest::Luid([0x26, 0x74, 1, 0, 0, 0, 0, 0])),
            Some(0)
        );
        assert_eq!(
            choose_device(&c, DeviceRequest::Luid([1, 0x78, 1, 0, 0, 0, 0, 0])),
            Some(1)
        );
        assert_eq!(choose_device(&c, DeviceRequest::Luid([9; 8])), None);
    }

    /// A device with everything the renderer needs, at the values of an RTX A2000 Laptop (NVIDIA 5xx driver).
    fn rtx_a2000() -> DeviceCaps {
        DeviceCaps {
            api_version: vk::make_api_version(0, 1, 4, 312),
            queue_family: true,
            needs_swapchain: true,
            swapchain: true,
            fragment_stores_and_atomics: true,
            dynamic_rendering: true,
            synchronization2: true,
            max_compute_shared_memory_size: 49152,
            max_compute_work_group_invocations: 1024,
            max_compute_work_group_size: [1024, 1024, 64],
        }
    }

    /// R-29: the device check names what is missing; the dev machine's GPUs and the usual desktop values pass,
    /// the Vulkan minimum-spec values do not.
    #[test]
    fn device_check_names_the_missing_limit_or_feature() {
        assert_eq!(device_meets(&rtx_a2000()), Ok(()));
        // Intel UHD (Gen 9 to 12) and AMD RDNA: 32 KiB and 64 KiB of shared memory.
        let intel = DeviceCaps {
            max_compute_shared_memory_size: 32768,
            ..rtx_a2000()
        };
        assert_eq!(device_meets(&intel), Ok(()));
        let headless = DeviceCaps {
            needs_swapchain: false,
            swapchain: false,
            ..rtx_a2000()
        };
        assert_eq!(device_meets(&headless), Ok(()), "no window: no swapchain needed");

        let minimum = DeviceCaps {
            max_compute_shared_memory_size: 16384,
            max_compute_work_group_invocations: 128,
            max_compute_work_group_size: [128, 128, 64],
            ..rtx_a2000()
        };
        let e = device_meets(&minimum).unwrap_err();
        assert!(e.contains("maxComputeWorkGroupInvocations is 128"), "{e}");
        let e = device_meets(&DeviceCaps {
            max_compute_work_group_invocations: 256,
            ..minimum
        })
        .unwrap_err();
        assert!(
            e.contains("maxComputeWorkGroupSize.x is 128, the shaders need 256"),
            "{e}"
        );
        let e = device_meets(&DeviceCaps {
            max_compute_work_group_invocations: 256,
            max_compute_work_group_size: [256, 16, 1],
            ..minimum
        })
        .unwrap_err();
        assert!(e.contains("maxComputeWorkGroupSize.y is 16"), "{e}");
        let e = device_meets(&DeviceCaps {
            max_compute_shared_memory_size: 16384,
            ..rtx_a2000()
        })
        .unwrap_err();
        assert!(e.contains("maxComputeSharedMemorySize is 16384 bytes"), "{e}");

        let cases: [(DeviceCaps, &str); 6] = [
            (
                DeviceCaps {
                    api_version: vk::API_VERSION_1_2,
                    ..rtx_a2000()
                },
                "Vulkan 1.2 only",
            ),
            (
                DeviceCaps {
                    queue_family: false,
                    ..rtx_a2000()
                },
                "graphics and compute and presents",
            ),
            (
                DeviceCaps {
                    swapchain: false,
                    ..rtx_a2000()
                },
                "VK_KHR_swapchain",
            ),
            (
                DeviceCaps {
                    fragment_stores_and_atomics: false,
                    ..rtx_a2000()
                },
                "fragmentStoresAndAtomics",
            ),
            (
                DeviceCaps {
                    dynamic_rendering: false,
                    ..rtx_a2000()
                },
                "dynamicRendering",
            ),
            (
                DeviceCaps {
                    synchronization2: false,
                    ..rtx_a2000()
                },
                "synchronization2",
            ),
        ];
        for (caps, want) in cases {
            let e = device_meets(&caps).unwrap_err();
            assert!(e.contains(want), "{want}: {e}");
        }
    }

    /// The limits the device check asks for cover every compute shader: workgroup sizes read from the sources
    /// (literal values, `#define` and `const int` names), and the shared memory of the three shaders that use the
    /// most, recomputed from their constants.
    #[test]
    fn needed_limits_cover_the_shaders() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../shaders");
        let read = |name: &str| {
            std::fs::read_to_string(dir.join(name))
                .expect("shader")
                .replace("\r\n", "\n")
        };
        let beam = read("include/beam.glsl");
        // The value of a name: `#define NAME v` or `const int NAME = v;` in `src` or beam.glsl.
        let value = |src: &str, name: &str| -> Option<u32> {
            for text in [src, beam.as_str()] {
                for line in text.lines() {
                    let line = line.trim();
                    let rest = line
                        .strip_prefix("#define ")
                        .and_then(|r| r.strip_prefix(name))
                        .filter(|r| r.starts_with(' '))
                        .or_else(|| {
                            line.strip_prefix("const int ")
                                .and_then(|r| r.strip_prefix(name))
                                .and_then(|r| r.trim_start().strip_prefix('='))
                        });
                    if let Some(v) = rest.and_then(|r| r.split(['u', ';', '/']).next()?.trim().parse().ok()) {
                        return Some(v);
                    }
                }
            }
            None
        };
        let size = |src: &str, key: &str| -> u32 {
            let Some(at) = src.find(key) else { return 1 };
            let token = src[at + key.len()..]
                .trim_start()
                .split([',', ')'])
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            token
                .parse()
                .ok()
                .or_else(|| value(src, &token))
                .unwrap_or_else(|| panic!("workgroup size `{token}` not resolved"))
        };
        let mut checked = 0;
        for entry in std::fs::read_dir(&dir).expect("shaders folder") {
            let path = entry.expect("entry").path();
            if path.extension().is_none_or(|e| e != "comp") {
                continue;
            }
            let src = std::fs::read_to_string(&path).expect("shader").replace("\r\n", "\n");
            let (x, y) = (size(&src, "local_size_x ="), size(&src, "local_size_y ="));
            let name = path.display();
            assert!(x * y <= NEEDED_INVOCATIONS, "{name}: {x} x {y} invocations");
            assert!(
                x <= NEEDED_GROUP_SIZE[0] && y <= NEEDED_GROUP_SIZE[1],
                "{name}: {x} x {y}"
            );
            checked += 1;
        }
        assert!(checked >= 20, "only {checked} compute shaders found");

        // composite.comp: 3 arrays of 2 N floats and one of N, N = (TILE + 2 HALO) S.
        let comp = read("composite.comp");
        for decl in [
            "shared float sh_sig[2 * N];",
            "shared float sh_flt[2 * N];",
            "shared float sh_b[2 * N];",
            "shared float sh_luma[N];",
        ] {
            assert!(comp.contains(decl), "composite.comp changed its shared arrays: {decl}");
        }
        assert_eq!(comp.matches("\nshared ").count(), 4, "composite.comp shared arrays");
        let n = (value(&comp, "TILE").unwrap() + 2 * value(&comp, "HALO").unwrap()) * value(&comp, "S").unwrap();
        assert!(7 * n * 4 <= NEEDED_SHARED_MEMORY, "composite.comp: {} bytes", 7 * n * 4);
        // beam.glsl: 5 vec3 arrays of BEAM_TILE_W x BEAM_TILE_H entries, 16 bytes each at worst.
        assert_eq!(
            beam.matches("\nshared vec3 beam_sh_").count(),
            5,
            "beam.glsl shared arrays"
        );
        let entries = value(&beam, "BEAM_TILE_W").unwrap() * value(&beam, "BEAM_TILE_H").unwrap();
        assert!(5 * entries * 16 <= NEEDED_SHARED_MEMORY, "beam tile");
        // optics_kernel.comp: (OPT_TAPS_MAX + 1)^2 floats.
        let k = read("optics_kernel.comp");
        let taps = value(&k, "OPT_TAPS_MAX").unwrap() + 1;
        assert!(taps * taps * 4 <= NEEDED_SHARED_MEMORY, "optics kernel");
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
