//! Adapter topology and GPU selection (HW-3, T-03, owner decision 2026-11-19).
//!
//! - Topology: the DXGI adapters with their outputs (monitors) and the Vulkan devices with their LUID.
//! - Selection ([`select`], pure): which adapter runs the whole chain (capture device and Vulkan renderer),
//!   from `--gpu`, the `[global.gpu] adapter` setting (`auto`, a LUID or a name part) and the rectangle the
//!   output window covers, with the warnings to show ([`GpuSelection`]).
//! - The selection in force and the adapter list for the settings menu: [`publish`], [`current`].
//! - `--list-adapters` ([`run`]): prints the topology, marks the chosen adapter, the capture adapter, a verdict
//!   and the warnings. Read-only: no window, no capture session, no logical Vulkan device, no rendering.

use std::{fmt, sync::OnceLock};

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

    /// The inverse of [`Luid::from_vulkan`].
    pub fn to_vulkan(self) -> [u8; 8] {
        let mut b = [0; 8];
        b[..4].copy_from_slice(&self.low.to_le_bytes());
        b[4..].copy_from_slice(&self.high.to_le_bytes());
        b
    }

    /// Parses the printed form `HHHHHHHH:LLLLLLLL` (high part, then low part, hexadecimal, 1 to 8 digits each,
    /// any case). `None` for anything else.
    pub fn parse(s: &str) -> Option<Self> {
        let (h, l) = s.trim().split_once(':')?;
        let hex = |p: &str| {
            (!p.is_empty() && p.len() <= 8 && p.chars().all(|c| c.is_ascii_hexdigit()))
                .then(|| u32::from_str_radix(p, 16).ok())
                .flatten()
        };
        Some(Self {
            high: hex(h)? as i32,
            low: hex(l)?,
        })
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

/// A rectangle in virtual-desktop pixels: left, top, right, bottom (right and bottom exclusive).
pub type Rect = (i32, i32, i32, i32);

/// What `[global.gpu] adapter` asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdapterSpec {
    /// The adapter owning the monitor the output covers.
    Auto,
    Luid(Luid),
    /// A case-insensitive part of the adapter name (DXGI or Vulkan name).
    Name(String),
}

impl AdapterSpec {
    /// `auto` (any case) or empty: [`AdapterSpec::Auto`]; `HHHHHHHH:LLLLLLLL`: a LUID; anything else: a name part.
    pub fn parse(s: &str) -> Self {
        let s = s.trim();
        if s.is_empty() || s.eq_ignore_ascii_case(crate::settings::GPU_ADAPTER_AUTO) {
            Self::Auto
        } else if let Some(l) = Luid::parse(s) {
            Self::Luid(l)
        } else {
            Self::Name(s.to_string())
        }
    }
}

impl fmt::Display for AdapterSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Auto => f.write_str(crate::settings::GPU_ADAPTER_AUTO),
            Self::Luid(l) => write!(f, "{l}"),
            Self::Name(n) => f.write_str(n),
        }
    }
}

/// A Vulkan device the renderer can use (Vulkan 1.3, graphics queue).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VulkanDevice {
    /// Vulkan enumeration index (`--gpu`, `--list-gpus`).
    pub index: usize,
    pub name: String,
    /// `None` when the driver reports no valid deviceLUID.
    pub luid: Option<Luid>,
}

/// Why the chosen adapter was chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// `--gpu <index>`.
    CommandLine,
    /// `[global.gpu] adapter` (a LUID or a name part).
    Setting,
    /// `auto`: the adapter owning the monitor the output covers.
    MonitorOwner,
    /// `auto` without a usable answer (monitor unknown, its adapter has no usable Vulkan device) or a request
    /// that matched nothing: the Vulkan default (NVIDIA discrete, else any discrete, else the first).
    Default,
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::CommandLine => "--gpu",
            Self::Setting => "gpu.adapter setting",
            Self::MonitorOwner => "auto: owns the monitor the output covers",
            Self::Default => "auto: default Vulkan device",
        })
    }
}

/// The DXGI adapter that owns a monitor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Owner {
    pub name: String,
    pub luid: Luid,
}

/// The adapter choice for the session (HW-3): read by the app, `--list-adapters` and, later, the menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuSelection {
    /// The Vulkan device the chain runs on; its LUID is also the capture device's adapter. `None` when no
    /// Vulkan device is usable.
    pub chosen: Option<VulkanDevice>,
    pub reason: Reason,
    /// The adapter owning the monitor the output covers, when known.
    pub owner: Option<Owner>,
    /// Problems to show (console `warning:` lines, later the menu).
    pub warnings: Vec<String>,
}

impl GpuSelection {
    /// The renderer request: by LUID when the chosen device has one, else by index.
    pub fn device_request(&self) -> crate::gpu::DeviceRequest {
        use crate::gpu::DeviceRequest;
        match &self.chosen {
            Some(VulkanDevice { luid: Some(l), .. }) => DeviceRequest::Luid(l.to_vulkan()),
            Some(v) => DeviceRequest::Index(v.index),
            None => DeviceRequest::Default,
        }
    }

    /// The adapter the capture's Direct3D 11 device must be created on (`None`: the default adapter).
    pub fn capture_luid(&self) -> Option<Luid> {
        self.chosen.as_ref().and_then(|v| v.luid)
    }

    /// One line: the chosen adapter and why.
    pub fn summary(&self) -> String {
        match &self.chosen {
            Some(v) => format!(
                "gpu: {} (Vulkan [{}], LUID {}; {})",
                v.name,
                v.index,
                v.luid.map_or_else(|| "none".to_string(), |l| l.to_string()),
                self.reason
            ),
            None => "gpu: no usable Vulkan device".to_string(),
        }
    }

    /// Records the device the renderer really got (it falls back to the default when the chosen one cannot
    /// present to the window): a different one updates `chosen` and adds a warning.
    pub fn note_renderer(&mut self, got: VulkanDevice) {
        let same = |c: &VulkanDevice| match (c.luid, got.luid) {
            (Some(a), Some(b)) => a == b,
            _ => c.index == got.index,
        };
        match &self.chosen {
            Some(c) if same(c) => return,
            Some(c) => self.warnings.push(format!(
                "the renderer could not use {} and runs on {} instead",
                c.name, got.name
            )),
            None => {}
        }
        if let (Some(o), Some(l)) = (&self.owner, got.luid) {
            if o.luid != l {
                self.warnings.push(crosses(&got.name, l, o));
            }
        }
        self.reason = Reason::Default;
        self.chosen = Some(got);
    }

    /// Records the adapter the capture device really got: a different one than the renderer's adds a warning.
    pub fn note_capture(&mut self, name: &str, luid: Luid) {
        if let Some(c) = &self.chosen {
            if c.luid.is_some_and(|l| l != luid) {
                self.warnings.push(format!(
                    "capture runs on {name} (LUID {luid}) but the renderer on {}: every captured frame crosses adapters",
                    c.name
                ));
            }
        }
    }
}

/// The warning for a chain that does not run on the adapter of the monitor the output covers.
fn crosses(name: &str, l: Luid, o: &Owner) -> String {
    format!(
        "the chain runs on {name} (LUID {l}) but the monitor the output covers is driven by {} (LUID {}): every presented frame crosses adapters",
        o.name, o.luid
    )
}

/// The adapter whose attached output overlaps `r` the most (`None` when no attached output overlaps it).
pub fn owner_of_rect(adapters: &[AdapterInfo], r: Rect) -> Option<&AdapterInfo> {
    let overlap = |o: Rect| {
        let w = (o.2.min(r.2) as i64 - o.0.max(r.0) as i64).max(0);
        let h = (o.3.min(r.3) as i64 - o.1.max(r.1) as i64).max(0);
        w * h
    };
    let mut best: Option<(&AdapterInfo, i64)> = None;
    for a in adapters {
        for o in a.outputs.iter().filter(|o| o.attached) {
            let area = overlap(o.rect);
            if area > 0 && best.is_none_or(|(_, b)| area > b) {
                best = Some((a, area));
            }
        }
    }
    best.map(|(a, _)| a)
}

/// The usable Vulkan devices whose name, or whose DXGI adapter's name (same LUID), contains `needle` (case
/// insensitive), in Vulkan order.
fn by_name<'a>(needle: &str, dxgi: &[AdapterInfo], vulkan: &'a [VulkanDevice]) -> Vec<&'a VulkanDevice> {
    let needle = needle.to_lowercase();
    let has = |s: &str| s.to_lowercase().contains(&needle);
    vulkan
        .iter()
        .filter(|v| has(&v.name) || v.luid.is_some_and(|l| dxgi.iter().any(|a| a.luid == l && has(&a.name))))
        .collect()
}

/// Chooses the adapter that runs the chain (pure, HW-3).
///
/// - `cli` (`--gpu <index>`) wins, then `setting` (`[global.gpu] adapter`); a request that matches no usable
///   Vulkan device warns and falls back to `auto`. A name part that matches several takes the first and warns.
/// - `auto`: the adapter owning the monitor that `covered` (the output window rectangle) overlaps most, matched
///   by LUID; when that is unknown or has no usable Vulkan device, `default` (the Vulkan index the renderer
///   picks without a request).
/// - Whatever the reason, a chosen adapter that is not the owner of the covered monitor warns (presenting
///   crosses adapters), as does a chosen device without a LUID (nothing can be checked).
pub fn select(
    cli: Option<usize>,
    setting: &AdapterSpec,
    dxgi: &[AdapterInfo],
    vulkan: &[VulkanDevice],
    default: Option<usize>,
    covered: Option<Rect>,
) -> GpuSelection {
    let mut warnings = Vec::new();
    let owner = covered.and_then(|r| owner_of_rect(dxgi, r)).map(|a| Owner {
        name: a.name.clone(),
        luid: a.luid,
    });
    let by_index = |i: usize| vulkan.iter().find(|v| v.index == i);
    let by_luid = |l: Luid| vulkan.iter().find(|v| v.luid == Some(l));
    const AUTO: &str = "using the automatic choice";

    let mut pick: Option<(&VulkanDevice, Reason)> = None;
    if let Some(i) = cli {
        match by_index(i) {
            Some(v) => pick = Some((v, Reason::CommandLine)),
            None => warnings.push(format!(
                "--gpu {i}: no usable Vulkan device has that index (see --list-gpus); {AUTO}"
            )),
        }
    } else {
        match setting {
            AdapterSpec::Auto => {}
            AdapterSpec::Luid(l) => match by_luid(*l) {
                Some(v) => pick = Some((v, Reason::Setting)),
                None => warnings.push(format!(
                    "gpu.adapter = \"{l}\": no usable Vulkan device has that LUID (see --list-adapters); {AUTO}"
                )),
            },
            AdapterSpec::Name(n) => match by_name(n, dxgi, vulkan).as_slice() {
                [] => warnings.push(format!(
                    "gpu.adapter = \"{n}\": no usable Vulkan device matches that name (see --list-adapters); {AUTO}"
                )),
                [v, rest @ ..] => {
                    if !rest.is_empty() {
                        warnings.push(format!(
                            "gpu.adapter = \"{n}\" matches several adapters ({}); using {} (give its LUID to be exact)",
                            std::iter::once(*v)
                                .chain(rest.iter().copied())
                                .map(|d| d.name.as_str())
                                .collect::<Vec<_>>()
                                .join(", "),
                            v.name
                        ));
                    }
                    pick = Some((v, Reason::Setting));
                }
            },
        }
    }

    if pick.is_none() {
        let fallback = || default.and_then(by_index).map(|v| (v, Reason::Default));
        pick = match &owner {
            Some(o) => match by_luid(o.luid) {
                Some(v) => Some((v, Reason::MonitorOwner)),
                None => {
                    warnings.push(format!(
                        "{} (LUID {}) drives the monitor the output covers but has no usable Vulkan device; using the default device",
                        o.name, o.luid
                    ));
                    fallback()
                }
            },
            None => {
                if covered.is_some() {
                    warnings.push(
                        "no adapter reports a monitor under the output window; using the default device".to_string(),
                    );
                }
                fallback()
            }
        };
    }

    match (pick, &owner) {
        (None, _) => warnings.push("no usable Vulkan 1.3 device".to_string()),
        (Some((v, _)), Some(o)) => match v.luid {
            Some(l) if l != o.luid => warnings.push(crosses(&v.name, l, o)),
            None => warnings.push(format!(
                "{} reports no LUID: cannot check it against the adapter of the monitor ({})",
                v.name, o.name
            )),
            Some(_) => {}
        },
        (Some((v, _)), None) if v.luid.is_none() => warnings.push(format!(
            "{} reports no LUID: the capture device uses the default adapter",
            v.name
        )),
        _ => {}
    }

    GpuSelection {
        reason: pick.map_or(Reason::Default, |(_, r)| r),
        chosen: pick.map(|(v, _)| v.clone()),
        owner,
        warnings,
    }
}

/// One adapter as the settings menu lists it (a combo; changing it needs a restart).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdapterEntry {
    pub name: String,
    pub luid: Option<Luid>,
    /// It has an output attached to the desktop.
    pub owns_monitors: bool,
    pub software: bool,
    /// The usable Vulkan device on it, `None` when it cannot run the chain.
    pub vulkan_index: Option<usize>,
}

impl AdapterEntry {
    /// The `[global.gpu] adapter` value that selects this entry: its LUID, else its name.
    pub fn setting_value(&self) -> String {
        self.luid.map_or_else(|| self.name.clone(), |l| l.to_string())
    }
}

/// The adapters for the menu: every DXGI adapter (with its usable Vulkan device, by LUID), then the usable
/// Vulkan devices that match no DXGI adapter.
pub fn entries(dxgi: &[AdapterInfo], vulkan: &[VulkanDevice]) -> Vec<AdapterEntry> {
    let mut out: Vec<AdapterEntry> = dxgi
        .iter()
        .map(|a| AdapterEntry {
            name: a.name.clone(),
            luid: Some(a.luid),
            owns_monitors: a.owns_monitors(),
            software: a.software,
            vulkan_index: vulkan.iter().find(|v| v.luid == Some(a.luid)).map(|v| v.index),
        })
        .collect();
    for v in vulkan {
        if !dxgi.iter().any(|a| Some(a.luid) == v.luid) {
            out.push(AdapterEntry {
                name: v.name.clone(),
                luid: v.luid,
                owns_monitors: false,
                software: false,
                vulkan_index: Some(v.index),
            });
        }
    }
    out
}

/// The DXGI adapters and the usable Vulkan devices, read once at start.
pub struct Topology {
    pub dxgi: Vec<AdapterInfo>,
    pub vulkan: Vec<VulkanDevice>,
    /// The Vulkan index the renderer picks without a request.
    pub default: Option<usize>,
}

impl Topology {
    /// Reads the topology: DXGI enumeration and a query-only Vulkan instance (no window, no device).
    pub fn read() -> Res<Self> {
        let dxgi = dxgi_adapters()?;
        let vk = gpu::vulkan_report()?;
        let vulkan = vk
            .gpus
            .iter()
            .filter(|(g, _)| vk.usable.contains(&g.index))
            .map(|(g, _)| VulkanDevice {
                index: g.index,
                name: g.name.clone(),
                luid: g.luid.map(Luid::from_vulkan),
            })
            .collect();
        Ok(Self {
            dxgi,
            vulkan,
            default: vk.picked,
        })
    }

    pub fn select(&self, cli: Option<usize>, setting: &AdapterSpec, covered: Option<Rect>) -> GpuSelection {
        select(cli, setting, &self.dxgi, &self.vulkan, self.default, covered)
    }

    pub fn entries(&self) -> Vec<AdapterEntry> {
        entries(&self.dxgi, &self.vulkan)
    }
}

/// The selection in force and the adapter list, published once the renderer runs.
#[derive(Clone, Debug)]
pub struct Current {
    pub selection: GpuSelection,
    pub adapters: Vec<AdapterEntry>,
    /// The `[global.gpu] adapter` value read at start (the menu shows a pending change against it).
    pub setting: String,
}

static CURRENT: OnceLock<Current> = OnceLock::new();

/// Publishes the session's selection (once; later calls are ignored: a change needs a restart).
pub fn publish(current: Current) {
    let _ = CURRENT.set(current);
}

/// The session's selection and adapter list, `None` before the renderer is created (or in tools without one).
pub fn current() -> Option<&'static Current> {
    CURRENT.get()
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

/// `ultimatecrt --list-adapters`: prints the whole report to stdout. `cli` is `--gpu`, `setting` the
/// `[global.gpu] adapter` value, `covered` the rectangle the output would cover (`None` when not known without a
/// window: then `auto` shows the default device and the per-monitor lines tell what it would pick).
pub fn run(cli: Option<usize>, setting: &str, covered: Option<Rect>) -> Res<()> {
    let topo = Topology::read()?;
    let spec = AdapterSpec::parse(setting);
    let sel = topo.select(cli, &spec, covered);
    let chosen_luid = sel.capture_luid();
    let mark = |l: Option<Luid>| {
        if l.is_some() && l == chosen_luid {
            "  <- CHOSEN"
        } else {
            ""
        }
    };
    let dxgi = &topo.dxgi;
    println!("DXGI adapters ({}):", dxgi.len());
    for a in dxgi {
        println!(
            "  [{}] {} (vendor 0x{:04X}, LUID {}){}{}",
            a.index,
            a.name,
            a.vendor_id,
            a.luid,
            if a.software { " software" } else { "" },
            mark(Some(a.luid))
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
        let chosen = sel.chosen.as_ref().is_some_and(|c| c.index == g.index);
        println!(
            "  [{}] {} (vendor 0x{:04X}, {:?}, Vulkan {}.{}, LUID {}){}{}",
            g.index,
            g.name,
            g.vendor_id,
            g.kind,
            ash::vk::api_version_major(g.api_version),
            ash::vk::api_version_minor(g.api_version),
            g.luid
                .map(|b| Luid::from_vulkan(b).to_string())
                .unwrap_or_else(|| "none (deviceLUIDValid false)".to_string()),
            if vk.usable.contains(&g.index) {
                ""
            } else {
                " not usable (needs Vulkan 1.3 and a graphics queue)"
            },
            if chosen { "  <- CHOSEN" } else { "" }
        );
        println!(
            "      external memory: VK_KHR_external_memory_win32 {}, base external memory {}; D3D11 texture handle: {}; D3D11 texture KMT handle: {}",
            yes_no(m.win32_extension),
            yes_no(m.base_extension),
            handle_support(m.d3d11_texture),
            handle_support(m.d3d11_texture_kmt)
        );
    }

    match vk
        .picked
        .and_then(|i| vk.gpus.iter().find(|(g, _)| g.index == i))
        .map(|(g, _)| g)
    {
        Some(g) => println!("Default Vulkan device (no request): [{}] {}", g.index, g.name),
        None => println!("Default Vulkan device (no request): none usable"),
    }

    println!(
        "Request: {}",
        match cli {
            Some(i) => format!("--gpu {i} (wins over gpu.adapter = \"{setting}\")"),
            None => format!(
                "gpu.adapter = \"{setting}\" (settings file; read as {})",
                match &spec {
                    AdapterSpec::Auto => "auto".to_string(),
                    AdapterSpec::Luid(l) => format!("the LUID {l}"),
                    AdapterSpec::Name(n) => format!("a name part \"{n}\""),
                }
            ),
        }
    );
    match covered {
        Some(r) => println!(
            "Output would cover {}x{} at ({}, {}), monitor driven by {}",
            r.2 - r.0,
            r.3 - r.1,
            r.0,
            r.1,
            sel.owner.as_ref().map_or("no adapter".to_string(), |o| o.name.clone())
        ),
        None => {
            println!("Output rectangle not known without a window (no --monitor, no remembered windowed rectangle)")
        }
    }
    println!("CHOSEN {}", sel.summary());
    if covered.is_none() && spec == AdapterSpec::Auto && cli.is_none() {
        for a in dxgi {
            for o in a.outputs.iter().filter(|o| o.attached) {
                let s = topo.select(None, &AdapterSpec::Auto, Some(o.rect));
                println!(
                    "  auto with the output on {}: {}",
                    o.device_name,
                    s.chosen.as_ref().map_or("no usable device", |c| c.name.as_str())
                );
            }
        }
    }

    let (capture_name, capture_luid) = crate::capture::capture_adapter(sel.capture_luid())?;
    println!("Capture Direct3D 11 device (on the chosen adapter): {capture_name} (LUID {capture_luid})");
    let mut sel = sel;
    sel.note_capture(&capture_name, capture_luid);
    let render = sel.chosen.as_ref().map(|c| (c.name.as_str(), c.luid));
    for line in verdict((&capture_name, capture_luid), render, dxgi) {
        println!("{line}");
    }
    for w in &sel.warnings {
        println!("WARNING: {w}");
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

    // The dev machine topology (QUESTIONS HW-3 result): RTX drives a 1920x1080 monitor at (0, 0), Intel a
    // 1920x1200 monitor at (1920, -116), the software adapter none. Vulkan enumerates the RTX first.
    const RTX: Luid = Luid { high: 0, low: 0x17801 };
    const INTEL: Luid = Luid { high: 0, low: 0x17426 };
    const RTX_MON: Rect = (0, 0, 1920, 1080);
    const INTEL_MON: Rect = (1920, -116, 3840, 1084);

    fn output(rect: Rect) -> OutputInfo {
        OutputInfo {
            device_name: String::new(),
            monitor_name: String::new(),
            rect,
            attached: true,
        }
    }

    fn dev_machine() -> (Vec<AdapterInfo>, Vec<VulkanDevice>) {
        let mut a = vec![
            adapter(0, "NVIDIA RTX A2000 Laptop GPU", RTX, &[]),
            adapter(1, "Intel(R) UHD Graphics", INTEL, &[]),
            adapter(2, "Microsoft Basic Render Driver", luid(0, 0x99), &[]),
        ];
        a[0].outputs.push(output(RTX_MON));
        a[1].outputs.push(output(INTEL_MON));
        a[2].software = true;
        let v = vec![
            VulkanDevice {
                index: 0,
                name: "NVIDIA RTX A2000 Laptop GPU".into(),
                luid: Some(RTX),
            },
            VulkanDevice {
                index: 1,
                name: "Intel(R) UHD Graphics 770".into(),
                luid: Some(INTEL),
            },
        ];
        (a, v)
    }

    fn chosen(s: &GpuSelection) -> Option<usize> {
        s.chosen.as_ref().map(|c| c.index)
    }

    #[test]
    fn luid_parses_the_printed_form_and_round_trips_through_vulkan_bytes() {
        assert_eq!(Luid::parse("00000000:00017801"), Some(RTX));
        assert_eq!(Luid::parse(" 0:17426 "), Some(INTEL));
        assert_eq!(Luid::parse("FFFFFFFF:abcdef01"), Some(luid(-1, 0xABCD_EF01)));
        for bad in ["", "17801", "x:1", "1:", ":1", "123456789:1", "1:2:3"] {
            assert_eq!(Luid::parse(bad), None, "{bad}");
        }
        assert_eq!(Luid::parse(&RTX.to_string()), Some(RTX));
        let l = luid(-2, 0x1234_5678);
        assert_eq!(Luid::from_vulkan(l.to_vulkan()), l);
    }

    #[test]
    fn spec_parses_auto_luid_and_name() {
        assert_eq!(AdapterSpec::parse("auto"), AdapterSpec::Auto);
        assert_eq!(AdapterSpec::parse(" AUTO "), AdapterSpec::Auto);
        assert_eq!(AdapterSpec::parse(""), AdapterSpec::Auto);
        assert_eq!(AdapterSpec::parse("00000000:00017801"), AdapterSpec::Luid(RTX));
        assert_eq!(AdapterSpec::parse("RTX"), AdapterSpec::Name("RTX".into()));
        assert_eq!(AdapterSpec::parse("Intel UHD").to_string(), "Intel UHD");
    }

    #[test]
    fn owner_is_the_adapter_with_the_largest_overlap() {
        let (a, _) = dev_machine();
        assert_eq!(owner_of_rect(&a, RTX_MON).unwrap().luid, RTX);
        assert_eq!(owner_of_rect(&a, INTEL_MON).unwrap().luid, INTEL);
        // A window straddling the two monitors, mostly on the Intel one.
        assert_eq!(owner_of_rect(&a, (1800, 100, 2600, 700)).unwrap().luid, INTEL);
        assert_eq!(owner_of_rect(&a, (100, 100, 1960, 700)).unwrap().luid, RTX);
        // Off every monitor, or touching an edge only.
        assert!(owner_of_rect(&a, (-500, -500, -100, -100)).is_none());
        assert!(owner_of_rect(&a, (-100, 0, 0, 100)).is_none());
        // A detached output owns nothing.
        let mut d = a.clone();
        d[0].outputs[0].attached = false;
        assert!(owner_of_rect(&d, RTX_MON).is_none());
    }

    #[test]
    fn auto_follows_the_monitor_the_output_covers() {
        let (a, v) = dev_machine();
        let s = select(None, &AdapterSpec::Auto, &a, &v, Some(0), Some(RTX_MON));
        assert_eq!((chosen(&s), s.reason), (Some(0), Reason::MonitorOwner));
        assert!(s.warnings.is_empty(), "{:?}", s.warnings);
        assert_eq!(s.capture_luid(), Some(RTX));
        let s = select(None, &AdapterSpec::Auto, &a, &v, Some(0), Some(INTEL_MON));
        assert_eq!((chosen(&s), s.reason), (Some(1), Reason::MonitorOwner));
        assert!(s.warnings.is_empty(), "{:?}", s.warnings);
        assert_eq!(s.capture_luid(), Some(INTEL));
        assert_eq!(s.device_request(), crate::gpu::DeviceRequest::Luid(INTEL.to_vulkan()));
    }

    #[test]
    fn auto_without_a_known_monitor_uses_the_default_device() {
        let (a, v) = dev_machine();
        let s = select(None, &AdapterSpec::Auto, &a, &v, Some(0), None);
        assert_eq!((chosen(&s), s.reason), (Some(0), Reason::Default));
        assert!(s.warnings.is_empty());
        assert!(s.owner.is_none());
        // A rectangle on no monitor warns.
        let s = select(
            None,
            &AdapterSpec::Auto,
            &a,
            &v,
            Some(0),
            Some((-900, -900, -800, -800)),
        );
        assert_eq!((chosen(&s), s.reason), (Some(0), Reason::Default));
        assert_eq!(s.warnings.len(), 1);
        assert!(s.warnings[0].contains("no adapter reports a monitor"));
    }

    #[test]
    fn auto_when_the_monitor_adapter_has_no_vulkan_device_falls_back_and_warns() {
        let (a, mut v) = dev_machine();
        v.retain(|d| d.luid != Some(INTEL));
        let s = select(None, &AdapterSpec::Auto, &a, &v, Some(0), Some(INTEL_MON));
        assert_eq!((chosen(&s), s.reason), (Some(0), Reason::Default));
        assert!(s.warnings[0].contains("has no usable Vulkan device"));
        // ... and the result does not drive that monitor: a second warning.
        assert!(s.warnings[1].contains("crosses adapters"), "{:?}", s.warnings);
    }

    #[test]
    fn setting_by_luid_or_name_wins_over_auto_and_warns_on_a_cross_adapter_present() {
        let (a, v) = dev_machine();
        let s = select(None, &AdapterSpec::Luid(RTX), &a, &v, Some(0), Some(INTEL_MON));
        assert_eq!((chosen(&s), s.reason), (Some(0), Reason::Setting));
        assert_eq!(s.owner.as_ref().unwrap().luid, INTEL);
        assert_eq!(s.warnings.len(), 1);
        assert!(s.warnings[0].contains("the chain runs on NVIDIA RTX A2000 Laptop GPU"));
        assert!(s.warnings[0].contains("crosses adapters"));
        // A name part, matched case-insensitively on the Vulkan or the DXGI name.
        let s = select(
            None,
            &AdapterSpec::Name("intel".into()),
            &a,
            &v,
            Some(0),
            Some(INTEL_MON),
        );
        assert_eq!((chosen(&s), s.reason), (Some(1), Reason::Setting));
        assert!(s.warnings.is_empty());
        let s = select(
            None,
            &AdapterSpec::Name("uhd graphics 770".into()),
            &a,
            &v,
            Some(0),
            None,
        );
        assert_eq!(chosen(&s), Some(1));
    }

    #[test]
    fn a_setting_that_matches_nothing_falls_back_to_auto_with_a_warning() {
        let (a, v) = dev_machine();
        let s = select(None, &AdapterSpec::Luid(luid(0, 5)), &a, &v, Some(0), Some(INTEL_MON));
        assert_eq!((chosen(&s), s.reason), (Some(1), Reason::MonitorOwner));
        assert_eq!(s.warnings.len(), 1);
        assert!(s.warnings[0].contains("no usable Vulkan device has that LUID"));
        let s = select(
            None,
            &AdapterSpec::Name("Radeon".into()),
            &a,
            &v,
            Some(0),
            Some(RTX_MON),
        );
        assert_eq!((chosen(&s), s.reason), (Some(0), Reason::MonitorOwner));
        assert!(s.warnings[0].contains("matches that name"));
        // The software adapter has no Vulkan device: same fallback.
        let s = select(
            None,
            &AdapterSpec::Name("Basic Render".into()),
            &a,
            &v,
            Some(0),
            Some(RTX_MON),
        );
        assert_eq!(chosen(&s), Some(0));
        assert_eq!(s.warnings.len(), 1);
    }

    #[test]
    fn an_ambiguous_name_takes_the_first_and_warns() {
        let (a, v) = dev_machine();
        // "i" is in both Vulkan device names.
        let s = select(None, &AdapterSpec::Name("i".into()), &a, &v, Some(0), Some(RTX_MON));
        assert_eq!(chosen(&s), Some(0));
        assert_eq!(s.warnings.len(), 1);
        assert!(s.warnings[0].contains("matches several adapters"));
    }

    #[test]
    fn command_line_index_wins_over_the_setting() {
        let (a, v) = dev_machine();
        let s = select(Some(1), &AdapterSpec::Luid(RTX), &a, &v, Some(0), Some(INTEL_MON));
        assert_eq!((chosen(&s), s.reason), (Some(1), Reason::CommandLine));
        assert!(s.warnings.is_empty());
        // An unknown index warns and falls back to auto (the setting is not consulted then).
        let s = select(Some(7), &AdapterSpec::Luid(RTX), &a, &v, Some(0), Some(INTEL_MON));
        assert_eq!((chosen(&s), s.reason), (Some(1), Reason::MonitorOwner));
        assert!(s.warnings[0].contains("--gpu 7"));
    }

    #[test]
    fn devices_without_luid_or_no_device_at_all() {
        let (a, _) = dev_machine();
        let v = vec![VulkanDevice {
            index: 0,
            name: "Old driver".into(),
            luid: None,
        }];
        let s = select(None, &AdapterSpec::Auto, &a, &v, Some(0), Some(RTX_MON));
        assert_eq!(chosen(&s), Some(0));
        assert_eq!(s.capture_luid(), None);
        assert_eq!(s.device_request(), crate::gpu::DeviceRequest::Index(0));
        assert!(s.warnings.iter().any(|w| w.contains("reports no LUID")));
        let s = select(None, &AdapterSpec::Auto, &a, &[], None, Some(RTX_MON));
        assert!(s.chosen.is_none());
        assert_eq!(s.device_request(), crate::gpu::DeviceRequest::Default);
        assert!(s.warnings.iter().any(|w| w.contains("no usable Vulkan 1.3 device")));
        assert_eq!(s.summary(), "gpu: no usable Vulkan device");
    }

    #[test]
    fn renderer_and_capture_notes() {
        let (a, v) = dev_machine();
        let mut s = select(None, &AdapterSpec::Auto, &a, &v, Some(0), Some(INTEL_MON));
        // The renderer got what was chosen: nothing changes.
        s.note_renderer(v[1].clone());
        assert!(s.warnings.is_empty());
        assert_eq!(s.reason, Reason::MonitorOwner);
        s.note_capture("Intel(R) UHD Graphics", INTEL);
        assert!(s.warnings.is_empty());
        // The renderer fell back to the RTX: chosen follows, two warnings (fallback, cross-adapter present).
        s.note_renderer(v[0].clone());
        assert_eq!((chosen(&s), s.reason), (Some(0), Reason::Default));
        assert_eq!(s.warnings.len(), 2, "{:?}", s.warnings);
        // A capture device left on another adapter.
        s.note_capture("Intel(R) UHD Graphics", INTEL);
        assert!(s.warnings[2].contains("every captured frame crosses adapters"));
        assert!(s
            .summary()
            .starts_with("gpu: NVIDIA RTX A2000 Laptop GPU (Vulkan [0], LUID 00000000:00017801;"));
    }

    #[test]
    fn menu_entries_list_every_adapter_with_its_vulkan_device() {
        let (a, mut v) = dev_machine();
        v.push(VulkanDevice {
            index: 2,
            name: "llvmpipe".into(),
            luid: None,
        });
        let e = entries(&a, &v);
        assert_eq!(e.len(), 4);
        assert_eq!((e[0].vulkan_index, e[0].owns_monitors), (Some(0), true));
        assert_eq!((e[1].vulkan_index, e[1].owns_monitors), (Some(1), true));
        assert_eq!((e[2].vulkan_index, e[2].software), (None, true));
        assert_eq!(e[3].name, "llvmpipe");
        assert_eq!(e[0].setting_value(), "00000000:00017801");
        assert_eq!(e[3].setting_value(), "llvmpipe");
        // The setting value selects the entry back.
        let s = select(None, &AdapterSpec::parse(&e[1].setting_value()), &a, &v, Some(0), None);
        assert_eq!(chosen(&s), Some(1));
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
