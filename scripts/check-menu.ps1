<#
.SYNOPSIS
    T-52a live check (NOT RUN by the agents): the settings menu focus hand-off of the real product, windowed
    and fullscreen, under the independent foreground poll. Prints PASS or FAIL per mode.
.DESCRIPTION
    !!! THIS SCRIPT OPENS WINDOWS, GOES FULLSCREEN AND SYNTHESISES KEY PRESSES (SendInput: the menu key, the
    !!! text "t52", Alt) AND MOVES THE FOREGROUND WINDOW. It needs a HANDS-OFF DESKTOP for about
    !!! 2 x (ClosedSeconds + Cycles x 3 s + 15 s), about 3 minutes with the defaults (12 minutes with
    !!! -ClosedSeconds 600, the plan's 10 minutes): do not touch the keyboard or the mouse, close any program
    !!! that holds the menu key as a global hotkey, and do not run it while another live task (desktop and GPU
    !!! token, PLAN section 3) runs. It refuses to start when the user was active in the last -MinIdle seconds
    !!! and reports ABORTED when user input happens during the run.

    Uses the menu hotkey of T-54 (`Action::ToggleMenu`, default F10, D-17), wired to `Ui::request_toggle`
    through `controls::UiHook`. If F10 fell back to another chord (see the `hotkeys:` console line), pass its
    key with -MenuKey (a modifier chord cannot be sent by this script).

    Steps per mode (windowed, fullscreen):
      1. start pixel-demo --scale 3 (the source) and give it the foreground (Alt tap + SetForegroundWindow, the
         script's own input);
      2. start scripts\poll-foreground.ps1 (T-06) as an independent 50 ms poll in a background job;
      3. start ultimatecrt --window pixel-demo [--fullscreen] --validation --exit-after T, output to files;
      4. menu closed for -ClosedSeconds: no foreground change is allowed, except T-07 hand-back blips (the output
         activated by Windows and the source back within 1 s), which are counted and reported;
      5. -Cycles times: menu key (SendInput) -> the foreground must be the output within 1 s; type "t52" (goes to
         ImGui; not verifiable from outside, see the manual checklist); menu key -> the foreground must be
         pixel-demo again within 1 s;
      6. stop everything; check the independent poll: exactly 2 changes per cycle plus 2 per hand-back blip;
         zero validation messages in the ultimatecrt error output.
    Output (-OutDir): the poll CSVs and the ultimatecrt console output. The CSVs hold window titles: delete them
    after reading. Exit 0 when every mode passes, 1 on a failure, 2 when not run (user active), 3 when aborted.

    Manual checklist (a person, after the automatic part, PLAN T-52a "slider and text input work"):
      - open the menu with the menu key: the cursor shows over the output, the menu has the keyboard focus;
      - drag the opacity slider and the exposure slider; the picture follows the exposure;
      - click into the preset filter and type: the text appears; pick a preset in the combo: the tube changes;
      - move and resize the menu, collapse it, close it with the menu key, reopen: same place and size;
        restart ultimatecrt: same place and size (settings.toml [global.ui]);
      - with the menu open, Alt+Tab to another window: the menu closes, the other window keeps the foreground;
      - with the menu open, Esc: the menu closes, the source gets the foreground back, the output stays open;
      - with the menu open, F11 (both directions): the mode changes, the menu stays open and usable; close the
        menu: the source has the foreground and the output does not take it again (T-07 hand-back only);
      - Ctrl+Alt+O shows and hides the overlay without any foreground change; with the menu open, Ctrl+Alt+Q
        quits and the source gets the foreground back;
      - with a hotkey made unavailable (another program holding F10), the menu shows the registration message.
.PARAMETER Bin
    Folder with ultimatecrt.exe and pixel-demo.exe. Default: target\release of the repository (built unless
    -NoBuild).
.PARAMETER Cycles
    Open and close cycles per mode. Default 10 (PLAN T-52a).
.PARAMETER ClosedSeconds
    Time with the menu closed before the cycles. Default 60; the plan's acceptance asks for 600.
.PARAMETER MenuKey
    Virtual-key code of the menu key sent with SendInput. Default 0x79 (F10, D-17).
#>
param(
    [string]$Bin = '',
    [int]$Cycles = 10,
    [int]$ClosedSeconds = 60,
    [int]$MenuKey = 0x79,
    [string[]]$Modes = @('windowed', 'fullscreen'),
    [string]$OutDir = (Join-Path $env:TEMP 'ultimatecrt-check-menu'),
    [int]$MinIdle = 60,
    [switch]$NoBuild
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$root = Split-Path $PSScriptRoot -Parent
if (-not $Bin) {
    $target = $env:CARGO_TARGET_DIR
    if (-not $target) { $target = Join-Path $root 'target' }
    $Bin = Join-Path $target 'release'
    if (-not $NoBuild) {
        Push-Location $root
        try {
            cargo build --release --locked -p pixel-demo -p ultimatecrt
            if ($LASTEXITCODE -ne 0) { throw 'build failed' }
        } finally { Pop-Location }
    }
}
$ucrt = Join-Path $Bin 'ultimatecrt.exe'
$demo = Join-Path $Bin 'pixel-demo.exe'
foreach ($exe in @($ucrt, $demo)) { if (-not (Test-Path $exe)) { throw "missing binary: $exe" } }
$poll = Join-Path $root 'scripts\poll-foreground.ps1'
New-Item -ItemType Directory -Force $OutDir | Out-Null

if (-not ('UcrtMenuCheck' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class UcrtMenuCheck {
    [StructLayout(LayoutKind.Sequential)] struct LII { public uint cbSize; public uint dwTime; }
    [DllImport("user32.dll")] static extern bool GetLastInputInfo(ref LII i);
    [DllImport("kernel32.dll")] static extern uint GetTickCount();
    public static double IdleSeconds() { var i = new LII(); i.cbSize = 8; GetLastInputInfo(ref i); return (GetTickCount() - i.dwTime) / 1000.0; }
    [DllImport("user32.dll")] static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
    public static string Title() { var sb = new StringBuilder(512); GetWindowText(GetForegroundWindow(), sb, sb.Capacity); return sb.ToString(); }
    [StructLayout(LayoutKind.Sequential)] struct KEYBDINPUT { public ushort wVk; public ushort wScan; public uint dwFlags; public uint time; public UIntPtr dwExtraInfo; }
    [StructLayout(LayoutKind.Explicit, Size = 40)] struct INPUT { [FieldOffset(0)] public uint type; [FieldOffset(8)] public KEYBDINPUT ki; }
    [DllImport("user32.dll")] static extern uint SendInput(uint n, INPUT[] inputs, int size);
    static uint Send(ushort vk, ushort scan, uint flags) {
        var i = new INPUT[1]; i[0].type = 1; i[0].ki.wVk = vk; i[0].ki.wScan = scan; i[0].ki.dwFlags = flags;
        return SendInput(1, i, Marshal.SizeOf(typeof(INPUT)));
    }
    public static void Tap(ushort vk) { Send(vk, 0, 0); Send(vk, 0, 2); }
    public static void Type(string s) { foreach (char c in s) { Send(0, c, 4); Send(0, c, 4 | 2); } }
    public static bool Activate(IntPtr h) { Tap(0x12); SetForegroundWindow(h); return GetForegroundWindow() == h; }
}
'@
}

$idle0 = [UcrtMenuCheck]::IdleSeconds()
if ($idle0 -lt $MinIdle) { Write-Output ("NOT RUN: user input {0:N0} s ago (needs {1} s idle)" -f $idle0, $MinIdle); exit 2 }

Write-Host '=================================================================='
Write-Host ' T-52a menu check: opens windows, goes fullscreen, sends the menu'
Write-Host ' key and text with SendInput. Hands off keyboard and mouse.'
Write-Host '=================================================================='

# Waits up to $ms for the foreground title to match $like; returns the title seen last.
function Wait-Foreground([string]$like, [int]$ms) {
    $sw = [Diagnostics.Stopwatch]::StartNew()
    do {
        $t = [UcrtMenuCheck]::Title()
        if ($t -like $like) { return $t }
        Start-Sleep -Milliseconds 20
    } while ($sw.ElapsedMilliseconds -lt $ms)
    return $t
}

$allPass = $true
$aborted = $false
foreach ($mode in $Modes) {
    Write-Host ''
    Write-Host "--- mode: $mode ($Cycles cycles, menu closed $ClosedSeconds s) ---"
    $reasons = @()
    $pollCsv = Join-Path $OutDir "menu-$mode-poll.csv"
    $outLog = Join-Path $OutDir "menu-$mode-stdout.txt"
    $errLog = Join-Path $OutDir "menu-$mode-stderr.txt"
    $run = $ClosedSeconds + $Cycles * 3 + 15

    $demoProc = Start-Process -FilePath $demo -ArgumentList @('--scale', '3') -PassThru
    Start-Sleep -Milliseconds 2000
    $demoProc.Refresh()
    [void][UcrtMenuCheck]::Activate($demoProc.MainWindowHandle)
    $injected = [DateTime]::Now
    $ucrtArgs = @('--window', 'pixel-demo', '--validation', '--exit-after', $run)
    if ($mode -eq 'fullscreen') { $ucrtArgs += '--fullscreen' }
    $up = Start-Process -FilePath $ucrt -ArgumentList $ucrtArgs -PassThru -RedirectStandardOutput $outLog -RedirectStandardError $errLog
    Start-Sleep -Milliseconds 3000
    # The independent poll starts once the output exists: from here on every change is counted.
    $job = Start-Job -ScriptBlock {
        param($p, $o, $d)
        & $p -OutFile $o -SourceTitle 'pixel-demo' -DurationSeconds $d -IntervalMs 50
    } -ArgumentList $poll, $pollCsv, ($run - 3)
    Start-Sleep -Milliseconds 1500

    # Menu closed: nothing may change the foreground (hand-back blips are counted from the poll CSV below).
    Start-Sleep -Seconds $ClosedSeconds
    if (([DateTime]::Now - $injected).TotalSeconds - [UcrtMenuCheck]::IdleSeconds() -gt 1) { $aborted = $true }

    $opens = 0; $closes = 0
    for ($c = 1; $c -le $Cycles; $c++) {
        [UcrtMenuCheck]::Tap([uint16]$MenuKey)
        $t = Wait-Foreground 'ultimatecrt*' 1000
        if ($t -like 'ultimatecrt*') { $opens++ } else { $reasons += "cycle ${c}: open left the foreground on '$t'" }
        Start-Sleep -Milliseconds 300
        [UcrtMenuCheck]::Type('t52')
        Start-Sleep -Milliseconds 700
        [UcrtMenuCheck]::Tap([uint16]$MenuKey)
        $t = Wait-Foreground '*pixel-demo*' 1000
        if ($t -like '*pixel-demo*') { $closes++ } else { $reasons += "cycle ${c}: close left the foreground on '$t'" }
        Start-Sleep -Milliseconds 800
    }
    $injected = [DateTime]::Now

    $pollOut = Receive-Job -Job $job -Wait -AutoRemoveJob 6>&1 2>&1 | Out-String
    if (-not $up.WaitForExit(30000)) { $up.Kill(); $reasons += 'ultimatecrt did not exit in time (killed)' }
    if (-not $demoProc.HasExited) { Stop-Process -Id $demoProc.Id -Force }

    Write-Host "  opens to the output: $opens / $Cycles, closes to the source: $closes / $Cycles"
    if ($opens -ne $Cycles) { $reasons += "opens $opens / $Cycles" }
    if ($closes -ne $Cycles) { $reasons += "closes $closes / $Cycles" }
    $handbacks = 0
    if (Test-Path $outLog) {
        $handbacks = @(Select-String -Path $outLog -Pattern 'focus: Windows activated the output (\d+)x' | ForEach-Object { [int]$_.Matches[0].Groups[1].Value } | Measure-Object -Sum).Sum
        if (-not $handbacks) { $handbacks = 0 }
        if (Select-String -Path $outLog -Pattern 'hotkey|registration' -Quiet) { Select-String -Path $outLog -Pattern 'hotkey|registration' | ForEach-Object { Write-Host "  $($_.Line)" } }
    }
    Write-Host "  T-07 hand-backs while the menu was closed: $handbacks"
    $m = [regex]::Match($pollOut, 'Foreground changes: (\d+)')
    if ($m.Success) {
        $n = [int]$m.Groups[1].Value
        $want = 2 * $Cycles + 2 * $handbacks
        Write-Host "  independent poll: $n foreground changes (expected $want)"
        if ($n -ne $want) { $reasons += "independent poll counted $n foreground changes, expected $want" }
    } else { $reasons += 'independent poll gave no result' }
    if (Test-Path $errLog) {
        $val = @(Select-String -Path $errLog -Pattern 'VUID|Validation|SYNC-HAZARD')
        if ($val.Count -gt 0) { $reasons += "validation messages: $($val.Count) (see $errLog)" }
        $errors = @(Select-String -Path $errLog -Pattern '^error|panicked')
        foreach ($e in $errors) { $reasons += "ultimatecrt: $($e.Line)" }
    }
    if ($reasons.Count -eq 0) { Write-Host "PASS ($mode)" } else {
        $allPass = $false
        Write-Host "FAIL ($mode):"
        $reasons | ForEach-Object { Write-Host "  - $_" }
    }
    Start-Sleep -Milliseconds 1500
}

Write-Host ''
Write-Host "Results in $OutDir (the CSV files hold window titles: delete them after reading)."
if ((([DateTime]::Now - $injected).TotalSeconds - [UcrtMenuCheck]::IdleSeconds()) -gt 2) { $aborted = $true }
if ($aborted) { Write-Host 'T-52a menu check: ABORTED (user input during the run)'; exit 3 }
if ($allPass) { Write-Host 'T-52a menu check: PASS (now run the manual checklist of the script header)'; exit 0 }
Write-Host 'T-52a menu check: FAIL'
exit 1
