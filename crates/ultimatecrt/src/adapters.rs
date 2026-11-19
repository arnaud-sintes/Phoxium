//! Adapter topology diagnostic (`--list-adapters`, T-03, HW-3).
//!
//! Prints the DXGI adapters with their outputs (monitors), the Vulkan devices with their LUID, the
//! adapter of the capture's Direct3D 11 device, the Vulkan device the renderer picks today, and a verdict
//! on whether capture and render share an adapter and which adapter owns the monitors. Read-only: no
//! window, no capture session, no logical Vulkan device, no rendering.

use std::fmt;

use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::LUID,
        Graphics::{
            Dxgi::{CreateDXGIFactory1, IDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE},
            Gdi::{EnumDisplayDevicesW, DISPLAY_DEVICEW},
        },
    },
};

use crate::{gpu, Res};

/// A Windows locally unique adapter identifier: the same value names an adapter in DXGI and Vulkan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Luid {
    pub high: i32,
    pub low: u32,
}

impl Luid {
    pub fn from_windows(l: LUID) -> Self {
        Self {
            high: l.HighPart,
            low: l.LowPart,
        }
    }

    /// From `VkPhysicalDeviceIDProperties::deviceLUID`: the bytes of a `LUID` in memory order, the low
    /// part (u32) first, then the high part (i32), both little-endian on Windows.
    pub fn from_vulkan(b: [u8; 8]) -> Self {
        Self {
            low: u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            high: i32::from_le_bytes([b[4], b[5], b[6], b[7]]),
        }
    }
}

impl fmt::Display for Luid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:08X}:{:08X}", self.high as u32, self.low)
    }
}

/// A monitor output of a DXGI adapter.
#[derive(Clone, Debug)]
pub struct OutputInfo {
    /// GDI device name, e.g. `\\.\DISPLAY1`.
    pub device_name: String,
    /// Monitor name from the display driver (EnumDisplayDevices), empty when unknown.
    pub monitor_name: String,
    /// Desktop rectangle (left, top, right, bottom) in virtual-desktop pixels.
    pub rect: (i32, i32, i32, i32),
    pub attached: bool,
}

/// A DXGI adapter with its outputs.
#[derive(Clone, Debug)]
pub struct AdapterInfo {
    pub index: usize,
    pub name: String,
    pub vendor_id: u32,
    pub luid: Luid,
    /// The Microsoft Basic Render Driver or another software adapter.
    pub software: bool,
    pub outputs: Vec<OutputInfo>,
}

impl AdapterInfo {
    /// True when the adapter has at least one output attached to the desktop.
    pub fn owns_monitors(&self) -> bool {
        self.outputs.iter().any(|o| o.attached)
    }
}

/// Outcome of comparing the capture adapter with the render adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Match {
    Same,
    Mismatch,
    /// The Vulkan device reports no valid LUID.
    Unknown,
}

/// Compares the LUID of the capture device with the LUID of the Vulkan device.
pub fn compare(capture: Luid, render: Option<Luid>) -> Match {
    match render {
        None => Match::Unknown,
        Some(r) if r == capture => Match::Same,
        Some(_) => Match::Mismatch,
    }
}

/// The adapters that have an output attached to the desktop (they compose and scan out the monitors).
pub fn monitor_owners(adapters: &[AdapterInfo]) -> Vec<&AdapterInfo> {
    adapters.iter().filter(|a| a.owns_monitors()).collect()
}

/// The verdict lines: capture against render, then who owns the monitors and how the capture and render
/// adapters relate to them. `render` is the picked Vulkan device as (name, LUID if valid).
pub fn verdict(capture: (&str, Luid), render: Option<(&str, Option<Luid>)>, adapters: &[AdapterInfo]) -> Vec<String> {
    let mut lines = Vec::new();
    let render_luid = render.and_then(|(_, l)| l);
    lines.push(match (render, compare(capture.1, render_luid)) {
        (None, _) => "VERDICT: no usable Vulkan device, nothing to compare with the capture adapter".to_string(),
        (_, Match::Same) => format!("VERDICT: capture and render on the same adapter (LUID {})", capture.1),
        (Some((rname, _)), Match::Mismatch) => format!(
            "VERDICT: WARNING capture on {} (LUID {}) but render on {} (LUID {}): every captured frame crosses adapters",
            capture.0,
            capture.1,
            rname,
            render_luid.expect("mismatch has a render LUID")
        ),
        (Some((rname, _)), Match::Unknown) => {
            format!("VERDICT: cannot compare: Vulkan device {rname} reports no valid LUID")
        }
    });
    let owners = monitor_owners(adapters);
    lines.push(match owners.as_slice() {
        [] => "MONITORS: no DXGI adapter reports an output attached to the desktop".to_string(),
        [a] => format!(
            "MONITORS: all attached outputs belong to {} (LUID {}){}",
            a.name,
            a.luid,
            if a.software { " (software adapter)" } else { "" }
        ),
        many => format!(
            "MONITORS: attached outputs are split across {}",
            many.iter()
                .map(|a| format!("{} (LUID {})", a.name, a.luid))
                .collect::<Vec<_>>()
                .join(" and ")
        ),
    });
    if !owners.is_empty() {
        let on_owner = |l: Option<Luid>| l.map(|l| owners.iter().any(|a| a.luid == l));
        for (what, l) in [("capture device", Some(capture.1)), ("render device", render_luid)] {
            lines.push(match on_owner(l) {
                Some(true) => format!("MONITORS: the {what} is on a monitor-owning adapter"),
                Some(false) => format!(
                    "MONITORS: the {what} is NOT on a monitor-owning adapter: its frames must be copied to the adapter that scans out the monitors"
                ),
                None => format!("MONITORS: the {what} has no valid LUID, cannot tell"),
            });
        }
    }
    if owners.len() > 1 {
        lines.push(
            "MONITORS: the monitors are split across adapters, so which monitor the output window covers decides whether presenting crosses adapters"
                .to_string(),
        );
    }
    lines
}

fn wide_to_string(w: &[u16]) -> String {
    let end = w.iter().position(|&c| c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(&w[..end])
}

/// Monitor name of a GDI display device (`\\.\DISPLAY1`): the display driver's string for the monitor on it.
fn monitor_name(device_name: &[u16]) -> String {
    let mut dd = DISPLAY_DEVICEW {
        cb: std::mem::size_of::<DISPLAY_DEVICEW>() as u32,
        ..Default::default()
    };
    let mut name: Vec<u16> = device_name.iter().copied().take_while(|&c| c != 0).collect();
    name.push(0);
    // SAFETY: `name` is a NUL-terminated UTF-16 string and `dd.cb` is set.
    let ok = unsafe { EnumDisplayDevicesW(PCWSTR(name.as_ptr()), 0, &mut dd, 0) };
    if ok.as_bool() {
        wide_to_string(&dd.DeviceString)
    } else {
        String::new()
    }
}

/// Enumerates the DXGI adapters and their outputs.
pub fn dxgi_adapters() -> Res<Vec<AdapterInfo>> {
    // SAFETY: plain COM calls on objects created here; every out structure is owned by this function.
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
        let mut adapters = Vec::new();
        let mut i = 0;
        while let Ok(adapter) = factory.EnumAdapters1(i) {
            let desc = adapter.GetDesc1()?;
            let mut outputs = Vec::new();
            let mut j = 0;
            while let Ok(output) = adapter.EnumOutputs(j) {
                let d = output.GetDesc()?;
                outputs.push(OutputInfo {
                    device_name: wide_to_string(&d.DeviceName),
                    monitor_name: monitor_name(&d.DeviceName),
                    rect: (
                        d.DesktopCoordinates.left,
                        d.DesktopCoordinates.top,
                        d.DesktopCoordinates.right,
                        d.DesktopCoordinates.bottom,
                    ),
                    attached: d.AttachedToDesktop.as_bool(),
                });
                j += 1;
            }
            adapters.push(AdapterInfo {
                index: i as usize,
                name: wide_to_string(&desc.Description),
                vendor_id: desc.VendorId,
                luid: Luid::from_windows(desc.AdapterLuid),
                software: desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0,
                outputs,
            });
            i += 1;
        }
        Ok(adapters)
    }
}

fn yes_no(b: bool) -> &'static str {
    if b {
        "yes"
    } else {
        "no"
    }
}

fn handle_support(s: Option<(bool, bool)>) -> String {
    match s {
        None => "not supported".to_string(),
        Some((i, e)) => format!("import {}, export {}", yes_no(i), yes_no(e)),
    }
}

/// `ultimatecrt --list-adapters`: prints the whole report to stdout.
pub fn run() -> Res<()> {
    let dxgi = dxgi_adapters()?;
    println!("DXGI adapters ({}):", dxgi.len());
    for a in &dxgi {
        println!(
            "  [{}] {} (vendor 0x{:04X}, LUID {}){}",
            a.index,
            a.name,
            a.vendor_id,
            a.luid,
            if a.software { " software" } else { "" }
        );
        if a.outputs.is_empty() {
            println!("      no outputs");
        }
        for o in &a.outputs {
            println!(
                "      output {} \"{}\": {}x{} at ({}, {}){}",
                o.device_name,
                o.monitor_name,
                o.rect.2 - o.rect.0,
                o.rect.3 - o.rect.1,
                o.rect.0,
                o.rect.1,
                if o.attached {
                    ""
                } else {
                    " (not attached to the desktop)"
                }
            );
        }
    }

    let vk = gpu::vulkan_report()?;
    println!("Vulkan devices ({}):", vk.gpus.len());
    for (g, m) in &vk.gpus {
        println!(
            "  [{}] {} (vendor 0x{:04X}, {:?}, Vulkan {}.{}, LUID {})",
            g.index,
            g.name,
            g.vendor_id,
            g.kind,
            ash::vk::api_version_major(g.api_version),
            ash::vk::api_version_minor(g.api_version),
            g.luid
                .map(|b| Luid::from_vulkan(b).to_string())
                .unwrap_or_else(|| "none (deviceLUIDValid false)".to_string())
        );
        println!(
            "      external memory: VK_KHR_external_memory_win32 {}, base external memory {}; D3D11 texture handle: {}; D3D11 texture KMT handle: {}",
            yes_no(m.win32_extension),
            yes_no(m.base_extension),
            handle_support(m.d3d11_texture),
            handle_support(m.d3d11_texture_kmt)
        );
    }

    let (capture_name, capture_luid) = crate::capture::capture_adapter()?;
    println!("Capture Direct3D 11 device (default adapter): {capture_name} (LUID {capture_luid})");
    let picked = vk
        .picked
        .and_then(|i| vk.gpus.iter().find(|(g, _)| g.index == i))
        .map(|(g, _)| g);
    match picked {
        Some(g) => println!(
            "Renderer Vulkan device today (no --gpu): [{}] {} (LUID {})",
            g.index,
            g.name,
            g.luid
                .map(|b| Luid::from_vulkan(b).to_string())
                .unwrap_or_else(|| "none".into())
        ),
        None => println!("Renderer Vulkan device today (no --gpu): none usable"),
    }
    let render = picked.map(|g| (g.name.as_str(), g.luid.map(Luid::from_vulkan)));
    for line in verdict((&capture_name, capture_luid), render, &dxgi) {
        println!("{line}");
    }
    println!("Not determined here: which adapter runs the captured game (see the Windows graphics settings).");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn luid(high: i32, low: u32) -> Luid {
        Luid { high, low }
    }

    fn adapter(index: usize, name: &str, l: Luid, outputs: &[bool]) -> AdapterInfo {
        AdapterInfo {
            index,
            name: name.into(),
            vendor_id: 0,
            luid: l,
            software: false,
            outputs: outputs
                .iter()
                .map(|&attached| OutputInfo {
                    device_name: String::new(),
                    monitor_name: String::new(),
                    rect: (0, 0, 10, 10),
                    attached,
                })
                .collect(),
        }
    }

    #[test]
    fn vulkan_luid_bytes_are_low_then_high() {
        let l = Luid::from_vulkan([0x78, 0x56, 0x34, 0x12, 0xFF, 0xFF, 0xFF, 0xFF]);
        assert_eq!(l, luid(-1, 0x1234_5678));
        assert_eq!(l.to_string(), "FFFFFFFF:12345678");
        assert_eq!(Luid::from_vulkan([1, 0, 0, 0, 0, 0, 0, 0]), luid(0, 1));
    }

    #[test]
    fn compare_matches_on_both_parts() {
        assert_eq!(compare(luid(0, 5), Some(luid(0, 5))), Match::Same);
        assert_eq!(compare(luid(0, 5), Some(luid(1, 5))), Match::Mismatch);
        assert_eq!(compare(luid(0, 5), Some(luid(0, 6))), Match::Mismatch);
        assert_eq!(compare(luid(0, 5), None), Match::Unknown);
    }

    #[test]
    fn owners_are_the_adapters_with_attached_outputs() {
        let a = [
            adapter(0, "Intel", luid(0, 1), &[true, true]),
            adapter(1, "NVIDIA", luid(0, 2), &[]),
            adapter(2, "Detached", luid(0, 3), &[false]),
        ];
        let o = monitor_owners(&a);
        assert_eq!(o.len(), 1);
        assert_eq!(o[0].name, "Intel");
    }

    #[test]
    fn verdict_same_adapter_on_the_monitor_adapter() {
        let a = [adapter(0, "NVIDIA", luid(0, 2), &[true])];
        let v = verdict(("NVIDIA", luid(0, 2)), Some(("NVIDIA", Some(luid(0, 2)))), &a);
        assert!(v[0].starts_with("VERDICT: capture and render on the same adapter (LUID 00000000:00000002)"));
        assert!(v[1].contains("all attached outputs belong to NVIDIA"));
        assert!(v[2].contains("capture device is on a monitor-owning adapter"));
        assert!(v[3].contains("render device is on a monitor-owning adapter"));
    }

    #[test]
    fn verdict_mismatch_with_monitors_on_the_igpu() {
        let a = [
            adapter(0, "Intel", luid(0, 1), &[true, true]),
            adapter(1, "NVIDIA", luid(0, 2), &[]),
        ];
        let v = verdict(("NVIDIA", luid(0, 2)), Some(("Intel", Some(luid(0, 1)))), &a);
        assert!(v[0].contains("WARNING capture on NVIDIA"));
        assert!(v[0].contains("render on Intel"));
        assert!(v[1].contains("belong to Intel"));
        assert!(v[2].contains("capture device is NOT on a monitor-owning adapter"));
        assert!(v[3].contains("render device is on a monitor-owning adapter"));
    }

    #[test]
    fn verdict_handles_missing_information() {
        let a = [
            adapter(0, "A", luid(0, 1), &[true]),
            adapter(1, "B", luid(0, 2), &[true]),
        ];
        let v = verdict(("A", luid(0, 1)), Some(("A", None)), &a);
        assert!(v[0].contains("cannot compare"));
        assert!(v[1].contains("split across A"));
        assert!(v[3].contains("render device has no valid LUID"));
        assert!(v[4].contains("split across adapters"));
        let v = verdict(("A", luid(0, 1)), None, &[]);
        assert!(v[0].contains("no usable Vulkan device"));
        assert!(v[1].contains("no DXGI adapter reports an output"));
        assert_eq!(v.len(), 2);
    }
}
