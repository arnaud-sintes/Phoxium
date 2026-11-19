<#
.SYNOPSIS
    Live focus regression check (T-07): runs pixel-demo and ultimatecrt for N seconds under the independent
    focus monitor and prints PASS or FAIL.
.DESCRIPTION
    Builds nothing. Starts scripts\focus-monitor.ps1 (WinEvent hook, own process), then pixel-demo --scale 4
    (the source, expected in the foreground), then ultimatecrt with -UcrtArgs plus --exit-after. FAIL when any
    foreground or focus event lands on an ultimatecrt window while it runs, or when the foreground at the end
    is not the window that had it before ultimatecrt started. Refuses to run (exit 2) if the user was active
    in the last -MinIdle seconds, and reports ABORTED if user input happens during the run (the result is
    then meaningless). Every process it started is stopped at the end.
    Output folder: the monitor CSV (titles can hold private content; delete it after review) and the
    ultimatecrt console output.
.EXAMPLE
    .\scripts\check-focus.ps1 -Bin .\target\release -Seconds 120 -UcrtArgs '--window pixel-demo --fullscreen'
#>
param(
    [Parameter(Mandatory = $true)][string]$Bin,
    [int]$Seconds = 120,
    [string]$UcrtArgs = '--window pixel-demo --fullscreen',
    [string]$OutDir = (Join-Path $env:TEMP 'ultimatecrt-check-focus'),
    [string]$Label = 'run',
    [int]$MinIdle = 60,
    [switch]$NoSource,
    [string]$Exe = 'ultimatecrt.exe'
)

$ErrorActionPreference = 'Stop'
$Bin = (Resolve-Path $Bin).Path
New-Item -ItemType Directory -Force $OutDir | Out-Null

if (-not ('UcrtIdle' -as [type])) {
    Add-Type -TypeDefinition @'
using System; using System.Runtime.InteropServices;
public static class UcrtIdle {
    [StructLayout(LayoutKind.Sequential)] struct LII { public uint cbSize; public uint dwTime; }
    [DllImport("user32.dll")] static extern bool GetLastInputInfo(ref LII i);
    [DllImport("kernel32.dll")] static extern uint GetTickCount();
    public static double Seconds() { var i = new LII(); i.cbSize = 8; GetLastInputInfo(ref i); return (GetTickCount() - i.dwTime) / 1000.0; }
}
'@
}

$idle0 = [UcrtIdle]::Seconds()
if ($idle0 -lt $MinIdle) { Write-Output ("NOT RUN: user input {0:N0} s ago (needs {1} s idle)" -f $idle0, $MinIdle); exit 2 }

$csv = Join-Path $OutDir "$Label-focus.csv"
$ucrtOut = Join-Path $OutDir "$Label-ucrt.txt"
$monitor = Join-Path $PSScriptRoot 'focus-monitor.ps1'
$started = @()
$t0 = Get-Date
try {
    $mon = Start-Process powershell -ArgumentList @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$monitor`"",
        '-OutFile', "`"$csv`"", '-DurationSeconds', ($Seconds + 12)) -NoNewWindow `
        -RedirectStandardOutput (Join-Path $OutDir "$Label-monitor.txt") -PassThru
    $started += $mon
    Start-Sleep -Seconds 3
    if (-not $NoSource) {
        $demo = Start-Process (Join-Path $Bin 'pixel-demo.exe') -ArgumentList '--scale 4' -PassThru
        $started += $demo
        Start-Sleep -Seconds 3
    }
    $ucrtStart = Get-Date
    $ucrt = Start-Process (Join-Path $Bin $Exe) -ArgumentList "$UcrtArgs --exit-after $Seconds" `
        -RedirectStandardOutput $ucrtOut -RedirectStandardError "$ucrtOut.err" -NoNewWindow -PassThru
    $started += $ucrt
    $aborted = $false
    while (-not $ucrt.HasExited -and ((Get-Date) - $ucrtStart).TotalSeconds -lt ($Seconds + 15)) {
        Start-Sleep -Seconds 1
        if ([UcrtIdle]::Seconds() -lt ((Get-Date) - $t0).TotalSeconds - 1) { $aborted = $true; break }
    }
    $ucrtEnd = Get-Date
    Start-Sleep -Seconds 2
} finally {
    foreach ($p in @($ucrt, $demo)) { if ($p -and -not $p.HasExited) { Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue } }
    if ($mon) { $null = $mon.WaitForExit(15000); if (-not $mon.HasExited) { Stop-Process -Id $mon.Id -Force -ErrorAction SilentlyContinue } }
}

$rows = Import-Csv $csv
$ucrtFrom = $ucrtStart.ToString('HH:mm:ss.fff'); $ucrtTo = $ucrtEnd.ToString('HH:mm:ss.fff')
$during = $rows | Where-Object { $_.time -ge $ucrtFrom -and $_.time -le $ucrtTo -and $_.event -ne 'end' }
$before = $rows | Where-Object { $_.time -lt $ucrtFrom -and $_.event -in 'baseline', 'foreground' } | Select-Object -Last 1
$bad = $during | Where-Object { $_.exe -ieq $Exe -and $_.event -in 'foreground', 'focus' }
$fgChanges = $during | Where-Object { $_.event -eq 'foreground' }
$secs = [math]::Round(($ucrtStart - $t0).TotalSeconds, 1)
Write-Output ("[$Label] ultimatecrt ran {0} .. {1}; monitor t_ms of start ~ {2:N0}" -f $ucrtFrom, $ucrtTo, ($secs * 1000))
Write-Output ("[$Label] foreground before: {0} ({1}); foreground changes during the run: {2}" -f $before.exe, $before.title, @($fgChanges).Count)
foreach ($r in $during) {
    $rel = ([datetime]::ParseExact($r.time, 'HH:mm:ss.fff', $null) - [datetime]::ParseExact($ucrtFrom, 'HH:mm:ss.fff', $null)).TotalSeconds
    Write-Output ("  +{0,6:N2} s  {1,-13} {2} {3,-28} {4,-18} noact={5} top={6} tool={7}" -f $rel, $r.event, $r.hwnd, $r.class, $r.exe, $r.noactivate, $r.topmost, $r.toolwindow)
}
$idle1 = [UcrtIdle]::Seconds()
Write-Output ("[$Label] idle before {0:N0} s, after {1:N0} s" -f $idle0, $idle1)
if ($aborted -or $idle1 -lt (($ucrtEnd - $t0).TotalSeconds)) { Write-Output "[$Label] ABORTED: user input during the run"; exit 3 }
if (@($bad).Count -gt 0 -or @($fgChanges).Count -gt 0) { Write-Output "[$Label] FAIL"; exit 1 }
Write-Output "[$Label] PASS"
exit 0
