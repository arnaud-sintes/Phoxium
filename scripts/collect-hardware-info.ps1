<#
.SYNOPSIS
    Collects the hardware facts that matter for Phoxium on a PC (GPUs and drivers, displays and refresh rates, cameras,
    Vulkan limits) into one text file. Read-only; no serial numbers, no computer or user name.
.PARAMETER PhoxiumExe
    Optional path of phoxium.exe (a release zip is enough): adds its --list-gpus and --list-adapters output.
.PARAMETER Out
    Output file (default: hardware-info.txt next to this script).
#>
param([string]$PhoxiumExe = '', [string]$Out = (Join-Path $PSScriptRoot 'hardware-info.txt'))
$ErrorActionPreference = 'Continue'
$lines = New-Object System.Collections.Generic.List[string]
function Add-Section($t) { $lines.Add(''); $lines.Add("== $t") }
function Add-Line($t) { $lines.Add("$t") }

Add-Section 'Machine'
$cs = Get-CimInstance Win32_ComputerSystem
Add-Line "Manufacturer / model: $($cs.Manufacturer) / $($cs.Model)"
$bios = Get-CimInstance Win32_BIOS
Add-Line "BIOS: $($bios.Manufacturer) $($bios.SMBIOSBIOSVersion) ($($bios.ReleaseDate))"
$os = Get-CimInstance Win32_OperatingSystem
Add-Line "OS: $($os.Caption) build $($os.BuildNumber) ($($os.OSArchitecture))"
Add-Line ("Power: " + ((Get-CimInstance Win32_Battery -ErrorAction SilentlyContinue | ForEach-Object { "battery status $($_.BatteryStatus) (2 = on AC), charge $($_.EstimatedChargeRemaining) %" }) -join '; '))

Add-Section 'CPU'
Get-CimInstance Win32_Processor | ForEach-Object { Add-Line "$($_.Name.Trim()): $($_.NumberOfCores) cores, $($_.NumberOfLogicalProcessors) threads, max $($_.MaxClockSpeed) MHz" }

Add-Section 'Memory'
$mods = Get-CimInstance Win32_PhysicalMemory
Add-Line ("Total: {0:N1} GB in {1} module(s)" -f (($mods | Measure-Object Capacity -Sum).Sum / 1GB), $mods.Count)
$mods | ForEach-Object { Add-Line ("  {0:N0} GB, {1} MHz (configured {2}), type code {3}" -f ($_.Capacity / 1GB), $_.Speed, $_.ConfiguredClockSpeed, $_.SMBIOSMemoryType) }

Add-Section 'Storage'
Get-PhysicalDisk | ForEach-Object { Add-Line ("{0}: {1:N0} GB, {2}, {3}" -f $_.FriendlyName, ($_.Size / 1GB), $_.MediaType, $_.BusType) }

Add-Section 'Graphics adapters (Windows)'
Get-CimInstance Win32_VideoController | ForEach-Object {
    Add-Line "$($_.Name): driver $($_.DriverVersion) ($($_.DriverDate)), PNP $($_.PNPDeviceID -replace '\\[^\\]*$','')"
    Add-Line "   current mode $($_.CurrentHorizontalResolution)x$($_.CurrentVerticalResolution) at $($_.CurrentRefreshRate) Hz (0 or null: not driving a display)"
}

Add-Section 'Displays and the refresh rates they offer'
Add-Type @'
using System; using System.Collections.Generic; using System.Runtime.InteropServices;
public static class DisplayInfo {
  [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)] public struct DEVMODE {
    [MarshalAs(UnmanagedType.ByValTStr, SizeConst=32)] public string dmDeviceName; public short dmSpecVersion, dmDriverVersion, dmSize, dmDriverExtra; public int dmFields;
    public int dmPositionX, dmPositionY, dmDisplayOrientation, dmDisplayFixedOutput; public short dmColor, dmDuplex, dmYResolution, dmTTOption, dmCollate;
    [MarshalAs(UnmanagedType.ByValTStr, SizeConst=32)] public string dmFormName; public short dmLogPixels; public int dmBitsPerPel, dmPelsWidth, dmPelsHeight, dmDisplayFlags, dmDisplayFrequency;
    public int dmICMMethod, dmICMIntent, dmMediaType, dmDitherType, dmReserved1, dmReserved2, dmPanningWidth, dmPanningHeight; }
  [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)] public struct DISPLAY_DEVICE {
    public int cb; [MarshalAs(UnmanagedType.ByValTStr, SizeConst=32)] public string DeviceName; [MarshalAs(UnmanagedType.ByValTStr, SizeConst=128)] public string DeviceString;
    public int StateFlags; [MarshalAs(UnmanagedType.ByValTStr, SizeConst=128)] public string DeviceID; [MarshalAs(UnmanagedType.ByValTStr, SizeConst=128)] public string DeviceKey; }
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern bool EnumDisplayDevices(string dev, uint i, ref DISPLAY_DEVICE d, uint flags);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern bool EnumDisplaySettings(string dev, int mode, ref DEVMODE dm);
  public static List<string> Run() {
    var res = new List<string>();
    for (uint i = 0; ; i++) {
      var d = new DISPLAY_DEVICE(); d.cb = Marshal.SizeOf(d);
      if (!EnumDisplayDevices(null, i, ref d, 0)) break;
      if ((d.StateFlags & 1) == 0) continue;   // attached to the desktop only
      var cur = new DEVMODE(); cur.dmSize = (short)Marshal.SizeOf(cur);
      EnumDisplaySettings(d.DeviceName, -1, ref cur);
      var rates = new SortedSet<int>(); int maxW = 0, maxH = 0;
      var dm = new DEVMODE(); dm.dmSize = (short)Marshal.SizeOf(dm);
      for (int m = 0; EnumDisplaySettings(d.DeviceName, m, ref dm); m++) {
        if (dm.dmPelsWidth == cur.dmPelsWidth && dm.dmPelsHeight == cur.dmPelsHeight) rates.Add(dm.dmDisplayFrequency);
        if (dm.dmPelsWidth * dm.dmPelsHeight > maxW * maxH) { maxW = dm.dmPelsWidth; maxH = dm.dmPelsHeight; }
      }
      res.Add(d.DeviceName + " on " + d.DeviceString + ": " + cur.dmPelsWidth + "x" + cur.dmPelsHeight + " at " + cur.dmDisplayFrequency + " Hz, position (" + cur.dmPositionX + "," + cur.dmPositionY + "); refresh rates offered at this size: " + string.Join(", ", rates) + "; largest mode " + maxW + "x" + maxH);
    }
    return res;
  }
}
'@
[DisplayInfo]::Run() | ForEach-Object { Add-Line $_ }
Get-CimInstance -Namespace root\wmi -ClassName WmiMonitorBasicDisplayParams -ErrorAction SilentlyContinue | ForEach-Object { Add-Line "Monitor panel size (cm): $($_.MaxHorizontalImageSize) x $($_.MaxVerticalImageSize), input $(if ($_.VideoInputType -eq 1) {'digital'} else {'analog'})" }
$hdr = Get-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\VideoSettings' -ErrorAction SilentlyContinue
Add-Line ("Windows HDR / auto colour settings key present: " + [bool]$hdr)

Add-Section 'Cameras and capture devices (Media Foundation / PnP)'
Get-PnpDevice -Class Camera, Image -PresentOnly -ErrorAction SilentlyContinue | ForEach-Object { Add-Line "$($_.FriendlyName) [$($_.Class)] status $($_.Status)" }

Add-Section 'NVIDIA (nvidia-smi)'
$smi = (Get-Command nvidia-smi.exe -ErrorAction SilentlyContinue).Source
if (-not $smi -and (Test-Path 'C:\Windows\System32\nvidia-smi.exe')) { $smi = 'C:\Windows\System32\nvidia-smi.exe' }
if ($smi) {
    & $smi '--query-gpu=name,driver_version,vbios_version,memory.total,power.limit,power.max_limit,clocks.max.graphics,clocks.max.memory,pcie.link.gen.max,display_mode,display_active,driver_model.current' '--format=csv' 2>&1 | ForEach-Object { Add-Line $_ }
} else { Add-Line 'nvidia-smi not found' }

Add-Section 'Vulkan (vulkaninfo, if the SDK is installed)'
$vk = Get-ChildItem "$env:VULKAN_SDK\Bin" -Filter 'vulkaninfo*.exe' -ErrorAction SilentlyContinue | Select-Object -First 1
if ($vk) {
    & $vk.FullName 2>&1 | Select-String -Pattern '^GPU\d|deviceName|apiVersion|driverVersion|driverName|deviceType|maxPerStageDescriptor(StorageImages|SampledImages|Samplers)|maxPerStageResources|maxImageDimension2D' | ForEach-Object { Add-Line $_.Line.Trim() }
} else { Add-Line 'vulkaninfo not found (install the Vulkan SDK, or give -PhoxiumExe)' }

if ($PhoxiumExe -and (Test-Path $PhoxiumExe)) {
    Add-Section 'Phoxium --list-gpus and --list-adapters'
    $tmp = Join-Path $env:TEMP ('phoxium-hw-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory $tmp | Out-Null
    $env:APPDATA = Join-Path $tmp 'appdata'; New-Item -ItemType Directory $env:APPDATA | Out-Null
    foreach ($a in '--list-gpus', '--list-adapters') {
        $o = Join-Path $tmp "$a.txt"
        $p = Start-Process $PhoxiumExe -ArgumentList $a -Wait -PassThru -WindowStyle Hidden -RedirectStandardOutput $o -RedirectStandardError (Join-Path $tmp "$a.err")
        Add-Line "-- phoxium $a (exit $($p.ExitCode))"
        Get-Content $o | ForEach-Object { Add-Line $_ }
    }
    try { [IO.Directory]::Delete($tmp, $true) } catch { }
}

[IO.File]::WriteAllText($Out, ($lines -join "`r`n"), (New-Object Text.UTF8Encoding $false))
"wrote $Out ($($lines.Count) lines)"
