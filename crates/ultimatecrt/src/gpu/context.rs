//! Vulkan instance, physical device choice and logical device with its graphics queue.
//!
//! A [`Context`] is created either for a window ([`Context::with_window`], the device must be able
//! to present to it) or without any surface ([`Context::new_headless`], offline rendering and
//! tests). Device selection lives only here.

use ash::{khr, vk, Device, Entry, Instance};

use super::Res;

pub struct GpuInfo {
    pub index: usize,
    pub name: String,
    pub kind: vk::PhysicalDeviceType,
    pub vendor_id: u32,
    pub api_version: u32,
}

const NVIDIA: u32 = 0x10DE;

/// Lists the Vulkan devices without creating a surface.
pub fn list_gpus() -> Res<Vec<GpuInfo>> {
    let entry = unsafe { Entry::load()? };
    let instance = unsafe { entry.create_instance(&vk::InstanceCreateInfo::default(), None)? };
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

fn gpu_info(instance: &Instance, pd: vk::PhysicalDevice, index: usize) -> GpuInfo {
    let p = unsafe { instance.get_physical_device_properties(pd) };
    GpuInfo {
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
}

impl Context {
    /// Creates a surface-less context (no window, no swapchain extension): NVIDIA discrete, else
    /// any discrete, else the first Vulkan 1.3 device with a graphics queue.
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
        let app = vk::ApplicationInfo::default()
            .application_name(c"ultimatecrt")
            .api_version(vk::API_VERSION_1_3);
        // Surface extensions are always enabled so that presenters can be added for new windows.
        let exts = [khr::surface::NAME.as_ptr(), khr::win32_surface::NAME.as_ptr()];
        let layers = [c"VK_LAYER_KHRONOS_validation".as_ptr()];
        let mut info = vk::InstanceCreateInfo::default()
            .application_info(&app)
            .enabled_extension_names(&exts);
        if validation {
            info = info.enabled_layer_names(&layers);
        }
        let instance = entry.create_instance(&info, None)?;
        let surface_fn = khr::surface::Instance::new(&entry, &instance);

        let surface = match window {
            Some((hwnd, hinstance)) => match create_win32_surface(&entry, &instance, hwnd, hinstance) {
                Ok(s) => Some(s),
                Err(e) => {
                    instance.destroy_instance(None);
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
                };
                Ok((ctx, surface))
            }
            Err(e) => {
                if let Some(s) = surface {
                    surface_fn.destroy_surface(s, None);
                }
                instance.destroy_instance(None);
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
        if g.api_version < vk::API_VERSION_1_3 {
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
        if let Some(f) = family {
            infos.push(g);
            handles.push((pd, f as u32));
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
    let device = instance.create_device(
        physical,
        &vk::DeviceCreateInfo::default()
            .queue_create_infos(&queues)
            .enabled_extension_names(dev_exts)
            .push_next(&mut f13),
        None,
    )?;
    Ok((g, physical, queue_family, device))
}

impl Drop for Context {
    fn drop(&mut self) {
        // SAFETY: the device and instance are owned by self and destroyed once, device first; the
        // owners of child objects destroy them before dropping the context (see the type doc).
        unsafe {
            let _ = self.device.device_wait_idle();
            self.device.destroy_device(None);
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

    /// Needs a Vulkan 1.3 driver; no window is created.
    #[test]
    fn headless_device() {
        let ctx = Context::new_headless(false).expect("headless Vulkan device");
        assert!(!ctx.gpu.name.is_empty());
        assert!(!ctx.can_present);
    }
}
