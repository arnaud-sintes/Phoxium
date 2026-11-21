<#
.SYNOPSIS
    Live focus regression check (T-07): runs pixel-demo and phoxium for N seconds under the independent
    focus monitor and prints PASS or FAIL.
.DESCRIPTION
    Builds nothing. Starts scripts\focus-monitor.ps1 (WinEvent hook, own process), then pixel-demo --scale 4
    (the source; put in the foreground with a synthetic Alt press and SetForegroundWindow, which is the
    script's own input and resets the idle clock), then phoxium (or -Exe from -Bin) with -PhoxiumArgs plus
    --exit-after. Result: PASS (exit 0) when no foreground change happens while it runs; MITIGATED (exit 4)
    when the only changes are hand-backs (the output became foreground and the previous window got it back
    within 1 s, no focus event on the output: the T-07 mitigation at work); FAIL (exit 1) otherwise.
    Refuses to run (exit 2) if the user was active in the last -MinIdle seconds, and reports ABORTED (exit 3)
    if user input happens after its own injection (the result is then meaningless). -ViaWmi starts the
    program through WMI in a new hidden console instead of this shell's console. Every process it started
    is stopped at the end.
    Output folder: the monitor CSV (titles can hold private content; delete it after review) and the
    phoxium console output.
.EXAMPLE
    .\scripts\check-focus.ps1 -Bin .\target\release -Seconds 120 -PhoxiumArgs '--window pixel-demo --fullscreen'
#>
param(
    [Parameter(Mandatory = $true)][string]$Bin,
    [int]$Seconds = 120,
    [string]$PhoxiumArgs = '--window pixel-demo --fullscreen',
    [string]$OutDir = (Join-Path $env:TEMP 'phoxium-check-focus'),
    [string]$Label = 'run',
    [int]$MinIdle = 60,
    [switch]$NoSource,
    [string]$Exe = 'phoxium.exe',
    [string]$DemoBin = '',
    [switch]$ViaWmi
)

$ErrorActionPreference = 'Stop'
$Bin = (Resolve-Path $Bin).Path
if (-not $DemoBin) { $DemoBin = $Bin }
New-Item -ItemType Directory -Force $OutDir | Out-Null

if (-not ('PhoxiumIdle' -as [type])) {
    Add-Type -TypeDefinition @'
using System; using System.Runtime.InteropServices;
public static class PhoxiumIdle {
    [StructLayout(LayoutKind.Sequential)] struct LII { public uint cbSize; public uint dwTime; }
    [DllImport("user32.dll")] static extern bool GetLastInputInfo(ref LII i);
    [DllImport("kernel32.dll")] static extern uint GetTickCount();
    public static double Seconds() { var i = new LII(); i.cbSize = 8; GetLastInputInfo(ref i); return (GetTickCount() - i.dwTime) / 1000.0; }
    [DllImport("user32.dll")] static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
    [DllImport("user32.dll")] static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] static extern IntPtr GetForegroundWindow();
    // Synthetic Alt press (resets the idle clock), then SetForegroundWindow: the usual way for a background
    // process to hand the foreground to a window it started.
    public static bool Activate(IntPtr h) { keybd_event(0x12, 0, 0, UIntPtr.Zero); keybd_event(0x12, 0, 2, UIntPtr.Zero); SetForegroundWindow(h); return GetForegroundWindow() == h; }
}
'@
}

$idle0 = [PhoxiumIdle]::Seconds()
if ($idle0 -lt $MinIdle) { Write-Output ("NOT RUN: user input {0:N0} s ago (needs {1} s idle)" -f $idle0, $MinIdle); exit 2 }

$csv = Join-Path $OutDir "$Label-focus.csv"
$phoxiumOut = Join-Path $OutDir "$Label-phoxium.txt"
$monitor = Join-Path $PSScriptRoot 'focus-monitor.ps1'
$started = @()
$t0 = Get-Date; $inject = $t0
try {
    $mon = Start-Process powershell -ArgumentList @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$monitor`"",
        '-OutFile', "`"$csv`"", '-DurationSeconds', ($Seconds + 12)) -NoNewWindow `
        -RedirectStandardOutput (Join-Path $OutDir "$Label-monitor.txt") -PassThru
    $started += $mon
    Start-Sleep -Seconds 3
    if (-not $NoSource) {
        $demo = Start-Process (Join-Path $DemoBin 'pixel-demo.exe') -ArgumentList '--scale 4' -PassThru
        $started += $demo
        Start-Sleep -Seconds 2
        $demo.Refresh(); $hDemo = $demo.MainWindowHandle
        $ok = [PhoxiumIdle]::Activate($hDemo); $inject = Get-Date
        Write-Output ("[$Label] own injection (Alt + SetForegroundWindow on pixel-demo) at {0}: foreground={1}" -f $inject.ToString('HH:mm:ss.fff'), $ok)
        Start-Sleep -Seconds 2
    }
    $phoxiumStart = Get-Date
    if ($ViaWmi) {
        # Detached from this shell's console: started by the WMI service in a new hidden console.
        $si = New-CimInstance -ClassName Win32_ProcessStartup -ClientOnly -Property @{ ShowWindow = [uint16]0 }
        $cmd = "cmd.exe /c `"`"$(Join-Path $Bin $Exe)`" $PhoxiumArgs --exit-after $Seconds > `"$phoxiumOut`" 2>&1`""
        $res = Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{ CommandLine = $cmd; ProcessStartupInformation = $si }
        Start-Sleep -Milliseconds 500
        $phoxium = Get-CimInstance Win32_Process -Filter "ParentProcessId=$($res.ProcessId)" | ForEach-Object { Get-Process -Id $_.ProcessId } | Select-Object -First 1
        if (-not $phoxium) { $phoxium = Get-Process -Id $res.ProcessId }
    } else {
        $phoxium = Start-Process (Join-Path $Bin $Exe) -ArgumentList "$PhoxiumArgs --exit-after $Seconds" `
            -RedirectStandardOutput $phoxiumOut -RedirectStandardError "$phoxiumOut.err" -NoNewWindow -PassThru
    }
    $started += $phoxium
    $aborted = $false
    while (-not $phoxium.HasExited -and ((Get-Date) - $phoxiumStart).TotalSeconds -lt ($Seconds + 15)) {
        Start-Sleep -Seconds 1
        if ([PhoxiumIdle]::Seconds() -lt ((Get-Date) - $inject).TotalSeconds - 1) { $aborted = $true; break }
    }
    $phoxiumEnd = Get-Date
    Start-Sleep -Seconds 2
} finally {
    foreach ($p in @($phoxium, $demo)) { if ($p -and -not $p.HasExited) { Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue } }
    if ($mon) { $null = $mon.WaitForExit(15000); if (-not $mon.HasExited) { Stop-Process -Id $mon.Id -Force -ErrorAction SilentlyContinue } }
}

$rows = Import-Csv $csv
$phoxiumFrom = $phoxiumStart.ToString('HH:mm:ss.fff'); $phoxiumTo = $phoxiumEnd.ToString('HH:mm:ss.fff')
$during = $rows | Where-Object { $_.time -ge $phoxiumFrom -and $_.time -le $phoxiumTo -and $_.event -ne 'end' }
$before = $rows | Where-Object { $_.time -lt $phoxiumFrom -and $_.event -in 'baseline', 'foreground' } | Select-Object -Last 1
$bad = $during | Where-Object { $_.exe -ieq $Exe -and $_.event -in 'foreground', 'focus' }
$fgChanges = $during | Where-Object { $_.event -eq 'foreground' }
$secs = [math]::Round(($phoxiumStart - $t0).TotalSeconds, 1)
Write-Output ("[$Label] phoxium ran {0} .. {1}; monitor t_ms of start ~ {2:N0}" -f $phoxiumFrom, $phoxiumTo, ($secs * 1000))
Write-Output ("[$Label] foreground before: {0} ({1}); foreground changes during the run: {2}" -f $before.exe, $before.title, @($fgChanges).Count)
foreach ($r in $during) {
    $rel = ([datetime]::ParseExact($r.time, 'HH:mm:ss.fff', $null) - [datetime]::ParseExact($phoxiumFrom, 'HH:mm:ss.fff', $null)).TotalSeconds
    Write-Output ("  +{0,6:N2} s  {1,-13} {2} {3,-28} {4,-18} noact={5} top={6} tool={7}" -f $rel, $r.event, $r.hwnd, $r.class, $r.exe, $r.noactivate, $r.topmost, $r.toolwindow)
}
$idle1 = [PhoxiumIdle]::Seconds()
Write-Output ("[$Label] idle before {0:N0} s, after {1:N0} s" -f $idle0, $idle1)
if ($aborted -or $idle1 -lt (($phoxiumEnd - $inject).TotalSeconds)) { Write-Output "[$Label] ABORTED: user input during the run"; exit 3 }
# A hand-back (T-07 mitigation): the output became foreground and the window that had the foreground
# before got it back within 1 s, with no keyboard focus on the output. Anything else is a failure.
$fg = @($fgChanges); $handedBack = 0; $other = 0; $i = 0
while ($i -lt $fg.Count) {
    $a = $fg[$i]; $b = if ($i + 1 -lt $fg.Count) { $fg[$i + 1] } else { $null }
    if ($a.exe -ieq $Exe -and $b -and $b.hwnd -eq $before.hwnd -and
        ([int64]$b.t_ms - [int64]$a.t_ms) -le 1000) { $handedBack++; $i += 2 } else { $other++; $i++ }
}
$focusOnOutput = @($during | Where-Object { $_.exe -ieq $Exe -and $_.event -eq 'focus' }).Count
Write-Output ("[$Label] handed back: {0}, other foreground changes: {1}, focus events on the output: {2}" -f $handedBack, $other, $focusOnOutput)
if ($other -gt 0 -or $focusOnOutput -gt 0) { Write-Output "[$Label] FAIL"; exit 1 }
if ($handedBack -gt 0) { Write-Output "[$Label] MITIGATED (foreground blips handed back, focus never moved)"; exit 4 }
Write-Output "[$Label] PASS"
exit 0
