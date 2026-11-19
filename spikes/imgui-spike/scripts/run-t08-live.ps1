<#
.SYNOPSIS
    T-08 live run: F12 ImGui menu focus hand-off and F11 toggle, unattended (windowed, then fullscreen).
.DESCRIPTION
    !!! THIS SCRIPT OPENS WINDOWS, GOES FULLSCREEN, REGISTERS THE GLOBAL HOTKEYS F11 AND F12, SYNTHESISES
    !!! KEY PRESSES (SendInput: F11, F12, Alt, the text "t08") AND MOVES THE FOREGROUND WINDOW.
    !!! It needs a hands-off desktop for about 2 x (Cycles x 4.2 s + 8 s), about 2 minutes with the default
    !!! 10 cycles: do not touch the keyboard or the mouse, close any program that uses F11 or F12 as a global
    !!! hotkey, and do not run it while another live task (desktop and GPU token, PLAN section 3) runs.

    Steps: builds `pixel-demo` (workspace root) and the spike's `live` binary in release; then for each mode
    (windowed, fullscreen): starts pixel-demo (the victim/source window, foreground), starts the independent
    foreground poll `scripts/poll-foreground.ps1` (T-06) in a background job, runs `live --selftest Cycles`
    (2 F11 toggles with the source focused, then Cycles x: F12 open, type "t08", F12 close), stops pixel-demo,
    and checks the pass criteria of PLAN T-08:
      - F11 and F12 registered as global hotkeys, F11 toggled twice while the source had the focus;
      - every open activated the output window (method recorded: plain, alt or attach);
      - the typed text reached the ImGui text field in every cycle;
      - every close returned the foreground to the source (pixel-demo);
      - no other foreground change: the live log has none outside the open/close hand-offs, and the
        independent poll counts exactly 2 changes per cycle;
      - zero Vulkan validation messages (when the layer is installed).
    Prints PASS or FAIL per mode with the reasons, exits 0 only when both modes pass.
    Output (CSV and summaries) goes to -OutDir. The CSVs hold window titles: delete them after reading.
.PARAMETER Cycles
    Open/close cycles per mode. Default 10.
.PARAMETER OutDir
    Result folder. Default: spikes\imgui-spike\live-results (git-ignored).
.PARAMETER FgMethod
    Foreground method passed to live: auto (default: plain SetForegroundWindow, then a synthetic Alt tap,
    then AttachThreadInput), plain, alt or attach. Run once with `plain` to learn whether the workarounds
    are needed at all.
.PARAMETER NoBuild
    Skip the builds (binaries already in the target folder).
#>
param(
    [int]$Cycles = 10,
    [string]$OutDir = '',
    [ValidateSet('auto', 'plain', 'alt', 'attach')][string]$FgMethod = 'auto',
    [switch]$NoBuild,
    [string[]]$Modes = @('windowed', 'fullscreen'),
    [ValidateSet('f12', 'f10', 'ctrl-alt-f12')][string]$MenuKey = 'f12'
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$spike = Split-Path -Parent $PSScriptRoot
$root = Split-Path -Parent (Split-Path -Parent $spike)
if ($OutDir -eq '') { $OutDir = Join-Path $spike 'live-results' }
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$OutDir = (Resolve-Path $OutDir).Path
$poll = Join-Path $root 'scripts\poll-foreground.ps1'
if (-not (Test-Path $poll)) { throw "poll script not found: $poll" }

Write-Host '=================================================================='
Write-Host ' T-08 live run: opens windows, goes fullscreen, sends F11/F12/Alt'
Write-Host ' and text with SendInput. Hands off keyboard and mouse until done.'
Write-Host '=================================================================='

# --- Build ---
$spikeTarget = $env:CARGO_TARGET_DIR
if (-not $spikeTarget) { $spikeTarget = Join-Path $spike 'target' }
if (-not $NoBuild) {
    Push-Location $spike
    try { cargo build --release --locked --bin live; if ($LASTEXITCODE -ne 0) { throw 'spike build failed' } } finally { Pop-Location }
    Push-Location $root
    try { cargo build --release --locked -p pixel-demo; if ($LASTEXITCODE -ne 0) { throw 'pixel-demo build failed' } } finally { Pop-Location }
}
$live = Join-Path $spikeTarget 'release\live.exe'
$rootTarget = $env:CARGO_TARGET_DIR
if (-not $rootTarget) { $rootTarget = Join-Path $root 'target' }
$demo = Join-Path $rootTarget 'release\pixel-demo.exe'
foreach ($exe in @($live, $demo)) { if (-not (Test-Path $exe)) { throw "missing binary: $exe" } }

if (-not ('T08Fg' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class T08Fg {
    [DllImport("user32.dll")] static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
    public static string Title() {
        StringBuilder sb = new StringBuilder(512);
        GetWindowText(GetForegroundWindow(), sb, sb.Capacity);
        return sb.ToString();
    }
}
'@
}

function Read-Summary([string]$path) {
    $h = @{}
    foreach ($line in Get-Content -LiteralPath $path) {
        $i = $line.IndexOf('=')
        if ($i -gt 0) { $h[$line.Substring(0, $i)] = $line.Substring($i + 1) }
    }
    return $h
}

# Foreground changes in the live CSV that are not one of the expected hand-offs: a change while the menu is
# open must land on the output window within 1 s of an `open` row; a change while it is closed must land on
# the source within 1 s of a `close` row. Everything else is a focus leak.
function Get-UnexpectedChanges([string]$csvPath) {
    $rows = Import-Csv -LiteralPath $csvPath
    # The poll thread and the main loop log independently: the foreground change can be logged up to
    # 200 ms before its `open` or `close` row (seen live: 1 ms before), so the window is [-200 ms, +1000 ms].
    $opens = @($rows | Where-Object { $_.kind -eq 'open' } | ForEach-Object { [int64]$_.t_ms })
    $closes = @($rows | Where-Object { $_.kind -eq 'close' } | ForEach-Object { [int64]$_.t_ms })
    $near = { param($t, $list) ($list | Where-Object { ($t - $_) -ge -200 -and ($t - $_) -le 1000 } | Measure-Object).Count -gt 0 }
    $bad = @()
    foreach ($r in $rows) {
        if ($r.kind -ne 'fg') { continue }
        $t = [int64]$r.t_ms
        if ($r.menu_open -eq '1') { $ok = ($r.fg_is_self -eq '1') -and (& $near $t $opens) }
        else { $ok = ($r.fg_is_source -eq '1') -and (& $near $t $closes) }
        if (-not $ok) { $bad += "t=$t ms fg='$($r.fg_title)' menu_open=$($r.menu_open)" }
    }
    return , $bad
}
$allPass = $true
foreach ($mode in $Modes) {
    Write-Host ''
    Write-Host "--- mode: $mode ($Cycles cycles, fg-method $FgMethod) ---"
    $csv = Join-Path $OutDir "t08-$mode.csv"
    $sum = Join-Path $OutDir "t08-$mode-summary.txt"
    $pollCsv = Join-Path $OutDir "t08-$mode-poll.csv"
    $reasons = @()

    $demoProc = Start-Process -FilePath $demo -ArgumentList @('--scale', '3') -PassThru -NoNewWindow
    Start-Sleep -Milliseconds 2000
    $title = [T08Fg]::Title()
    if ($title -notlike '*pixel-demo*') {
        Write-Host "WARNING: the foreground is '$title', not pixel-demo; click nothing, the run continues."
    }
    $duration = [int]($Cycles * 4.2 + 12)
    $job = Start-Job -ScriptBlock {
        param($p, $o, $d)
        & $p -OutFile $o -SourceTitle 'pixel-demo' -DurationSeconds $d -IntervalMs 50
    } -ArgumentList $poll, $pollCsv, $duration

    $liveArgs = @('--selftest', $Cycles, '--csv', $csv, '--summary', $sum, '--source-title', 'pixel-demo', '--fg-method', $FgMethod, '--menu-key', $MenuKey)
    if ($mode -eq 'fullscreen') { $liveArgs += '--fullscreen' }
    $lp = Start-Process -FilePath $live -ArgumentList $liveArgs -PassThru -NoNewWindow
    if (-not $lp.WaitForExit(($duration + 30) * 1000)) {
        $lp.Kill()
        $reasons += 'live did not exit in time (killed)'
    }
    $pollOut = Receive-Job -Job $job -Wait -AutoRemoveJob 6>&1 2>&1 | Out-String
    if (-not $demoProc.HasExited) { Stop-Process -Id $demoProc.Id -Force }

    if (-not (Test-Path $sum)) {
        $reasons += 'no summary written'
    } else {
        $s = Read-Summary $sum
        Get-Content -LiteralPath $sum | ForEach-Object { Write-Host "  $_" }
        if ($s['f11_registered'] -ne 'true') { $reasons += 'F11 hotkey not registered' }
        if ($s['f12_registered'] -ne 'true') { $reasons += 'F12 hotkey not registered (reserved for the debugger on some systems)' }
        if ([int]$s['f11_toggles'] -ne 2) { $reasons += "F11 toggles $($s['f11_toggles']) (expected 2)" }
        foreach ($k in @('opens', 'opens_ok', 'closes', 'closes_ok', 'closes_to_source', 'text_ok')) {
            if ([int]$s[$k] -ne $Cycles) { $reasons += "$k = $($s[$k]) (expected $Cycles)" }
        }
        if ($s['validation'] -eq 'true' -and [int]$s['validation_messages'] -ne 0) { $reasons += "validation messages: $($s['validation_messages'])" }
        if ($s['validation'] -ne 'true') { Write-Host '  note: validation layer not used (not installed?)' }
        if ($s['error']) { $reasons += "live error: $($s['error'])" }
    }
    if (Test-Path $csv) {
        $bad = Get-UnexpectedChanges $csv
        foreach ($b in $bad) { $reasons += "unexpected foreground change: $b" }
    } else { $reasons += 'no live CSV' }
    $m = [regex]::Match($pollOut, 'Foreground changes: (\d+)')
    if ($m.Success) {
        $n = [int]$m.Groups[1].Value
        Write-Host "  independent poll: $n foreground changes (expected $(2 * $Cycles))"
        if ($n -ne 2 * $Cycles) { $reasons += "independent poll counted $n foreground changes, expected $(2 * $Cycles)" }
    } else { $reasons += 'independent poll gave no result' }

    if ($reasons.Count -eq 0) {
        Write-Host "PASS ($mode)"
    } else {
        $allPass = $false
        Write-Host "FAIL ($mode):"
        $reasons | ForEach-Object { Write-Host "  - $_" }
    }
    Start-Sleep -Milliseconds 1500
}

Write-Host ''
Write-Host "Results in $OutDir (CSV files hold window titles: delete them after reading)."
if ($allPass) { Write-Host 'T-08 live: PASS'; exit 0 }
Write-Host 'T-08 live: FAIL'
exit 1
