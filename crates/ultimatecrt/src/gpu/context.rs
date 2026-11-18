//! Vulkan instance, physical device enumeration and the surface-less device context.

use ash::{vk, Entry, Instance};

use super::Res;

pub struct GpuInfo {
    pub index: usize,
    pub name: String,
    pub kind: vk::PhysicalDeviceType,
    pub vendor_id: u32,
    pub api_version: u32,
}

/// Lists the Vulkan devices without creating a surface.
pub fn list_gpus() -> Res<Vec<GpuInfo>> {
    let entry = unsafe { Entry::load()? };
    let instance = unsafe { entry.create_instance(&vk::InstanceCreateInfo::default(), None)? };
    let out = unsafe { instance.enumerate_physical_devices()? }
        .into_iter()
        .enumerate()
        .map(|(index, pd)| gpu_info(&instance, pd, index))
        .collect();
    unsafe { instance.destroy_instance(None) };
    Ok(out)
}

pub(super) fn gpu_info(instance: &Instance, pd: vk::PhysicalDevice, index: usize) -> GpuInfo {
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

/// Vulkan instance, device and graphics queue created without any surface (offline rendering,
/// tests). Selection: NVIDIA discrete, else any discrete, else the first Vulkan 1.3 device.
#[allow(dead_code)] // Used by T-09 (offline harness).
pub struct Context {
    pub entry: Entry,
    pub instance: Instance,
    pub physical: vk::PhysicalDevice,
    pub device: ash::Device,
    pub queue_family: u32,
    pub queue: vk::Queue,
    pub gpu: GpuInfo,
}

#[allow(dead_code)] // Used by T-09 (offline harness).
impl Context {
    /// Creates a surface-less Vulkan 1.3 device with dynamic rendering and synchronization2.
    pub fn new_headless(validation: bool) -> Res<Self> {
        // SAFETY: every Vulkan object below is created from a live parent and, on success, owned
        // by the returned Context which destroys them in reverse order in Drop.
        unsafe {
            let entry = Entry::load()?;
            let app = vk::ApplicationInfo::default()
                .application_name(c"ultimatecrt")
                .api_version(vk::API_VERSION_1_3);
            let layers = [c"VK_LAYER_KHRONOS_validation".as_ptr()];
            let mut info = vk::InstanceCreateInfo::default().application_info(&app);
            if validation {
                info = info.enabled_layer_names(&layers);
            }
            let instance = entry.create_instance(&info, None)?;
            let mut candidates = Vec::new();
            for (i, pd) in instance.enumerate_physical_devices()?.into_iter().enumerate() {
                let g = gpu_info(&instance, pd, i);
                let family = instance
                    .get_physical_device_queue_family_properties(pd)
                    .iter()
                    .position(|q| q.queue_flags.contains(vk::QueueFlags::GRAPHICS));
                if let (Some(f), true) = (family, g.api_version >= vk::API_VERSION_1_3) {
                    candidates.push((g, pd, f as u32));
                }
            }
            let chosen = candidates
                .iter()
                .position(|c| c.0.vendor_id == 0x10DE && c.0.kind == vk::PhysicalDeviceType::DISCRETE_GPU)
                .or_else(|| {
                    candidates
                        .iter()
                        .position(|c| c.0.kind == vk::PhysicalDeviceType::DISCRETE_GPU)
                })
                .or(if candidates.is_empty() { None } else { Some(0) });
            let Some(chosen) = chosen else {
                instance.destroy_instance(None);
                return Err("no Vulkan 1.3 device with a graphics queue".into());
            };
            let (gpu, physical, queue_family) = candidates.swap_remove(chosen);
            let priorities = [1.0f32];
            let queues = [vk::DeviceQueueCreateInfo::default()
                .queue_family_index(queue_family)
                .queue_priorities(&priorities)];
            let mut f13 = vk::PhysicalDeviceVulkan13Features::default()
                .dynamic_rendering(true)
                .synchronization2(true);
            let device = match instance.create_device(
                physical,
                &vk::DeviceCreateInfo::default()
                    .queue_create_infos(&queues)
                    .push_next(&mut f13),
                None,
            ) {
                Ok(d) => d,
                Err(e) => {
                    instance.destroy_instance(None);
                    return Err(e.into());
                }
            };
            let queue = device.get_device_queue(queue_family, 0);
            Ok(Self {
                entry,
                instance,
                physical,
                device,
                queue_family,
                queue,
                gpu,
            })
        }
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        // SAFETY: the device and instance are owned by self and destroyed once, device first.
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

    /// Needs a Vulkan 1.3 driver; no window is created.
    #[test]
    fn headless_device() {
        let ctx = Context::new_headless(false).expect("headless Vulkan device");
        assert!(!ctx.gpu.name.is_empty());
    }
}
