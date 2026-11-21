<#
.SYNOPSIS
    Live check of the hotkeys and the output modes (T-54, T-57): NOT RUN by the build agent.
.DESCRIPTION
    NEEDS A HANDS-OFF DESKTOP. The script sends real key presses with SendInput (F11, Ctrl+Alt+F7,
    Ctrl+Alt+PageDown, Ctrl+Alt+PageUp, Ctrl+Alt+End), so any key you press or any window you activate while it
    runs falsifies the result. It refuses to start unless the desktop was idle for -MinIdle seconds, and it
    reports ABORTED if it sees user input between its own injections. It opens windows (pixel-demo and the
    phoxium output, fullscreen on the source's monitor), so do not run it while working.

    Steps:
      1. Starts scripts\focus-monitor.ps1 (independent WinEvent hook), then pixel-demo (the source, put in the
         foreground), then phoxium --fullscreen.
      2. Checks the console line "hotkeys:" (every chord registered, none "unavailable") and lists the
         registration warnings (a chord that fell back: the F10 case of PLAN D-17 names its Win32 error).
      3. F11 six times (3 windowed/fullscreen cycles): after each press the console must report the new mode
         and the window style must match (framed windowed, borderless fullscreen). The monitor CSV must show NO
         foreground or focus event for the output in the 1.5 s after a press, except the documented T-07
         hand-back (the output becomes foreground and the source gets it back within 1 s: counted separately).
      4. Ctrl+Alt+F7 twice (bypass on, then off), Ctrl+Alt+PageDown and Ctrl+Alt+PageUp (next and previous
         preset): the console must print the bypass state and the preset names. The menu key (F10 by default)
         is NOT pressed: with the settings menu wired in (T-52a) it activates the output on purpose. Check it
         by hand.
      5. Ctrl+Alt+End: phoxium must exit within 3 s. (The default chords since review D-01; the Ctrl+Alt+letter
         chords of earlier builds also fired on AltGr. A settings.toml that rebinds them makes this check fail.)
    Prints PASS (exit 0), MITIGATED (exit 4: only T-07 hand-backs seen), FAIL (exit 1), NOT RUN (exit 2: desktop
    not idle) or ABORTED (exit 3: user input during the run). Every process it started is stopped at the end.
    The monitor CSV holds window titles: delete the output folder after review.
.EXAMPLE
    .\scripts\check-hotkeys.ps1 -Bin .\target\release
#>
param(
    [Parameter(Mandatory = $true)][string]$Bin,
    [string]$PhoxiumArgs = '--window pixel-demo --fullscreen',
    [string]$OutDir = (Join-Path $env:TEMP 'phoxium-check-hotkeys'),
    [int]$MinIdle = 60,
    [string]$Exe = 'phoxium.exe',
    [string]$DemoBin = ''
)

$ErrorActionPreference = 'Stop'
$Bin = (Resolve-Path $Bin).Path
if (-not $DemoBin) { $DemoBin = $Bin }
New-Item -ItemType Directory -Force $OutDir | Out-Null

if (-not ('PhoxiumKeys' -as [type])) {
    Add-Type -TypeDefinition @'
using System; using System.Runtime.InteropServices;
public static class PhoxiumKeys {
    [StructLayout(LayoutKind.Sequential)] struct MOUSEINPUT { public int dx, dy; public uint mouseData, dwFlags, time; public IntPtr extra; }
    [StructLayout(LayoutKind.Sequential)] struct KEYBDINPUT { public ushort vk, scan; public uint flags, time; public IntPtr extra; }
    [StructLayout(LayoutKind.Explicit)] struct INPUTU { [FieldOffset(0)] public MOUSEINPUT mi; [FieldOffset(0)] public KEYBDINPUT ki; }
    [StructLayout(LayoutKind.Sequential)] struct INPUT { public uint type; public INPUTU u; }
    [DllImport("user32.dll", SetLastError = true)] static extern uint SendInput(uint n, INPUT[] inputs, int size);
    [StructLayout(LayoutKind.Sequential)] struct LII { public uint cbSize; public uint dwTime; }
    [DllImport("user32.dll")] static extern bool GetLastInputInfo(ref LII i);
    [DllImport("kernel32.dll")] static extern uint GetTickCount();
    [DllImport("user32.dll")] static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
    [DllImport("user32.dll")] static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll", EntryPoint = "GetWindowLongPtrW")] static extern IntPtr GetWindowLongPtr(IntPtr h, int idx);
    public static double IdleSeconds() { var i = new LII(); i.cbSize = 8; GetLastInputInfo(ref i); return (GetTickCount() - i.dwTime) / 1000.0; }
    public static bool Activate(IntPtr h) { keybd_event(0x12, 0, 0, UIntPtr.Zero); keybd_event(0x12, 0, 2, UIntPtr.Zero); SetForegroundWindow(h); return GetForegroundWindow() == h; }
    public static long Style(IntPtr h) { return GetWindowLongPtr(h, -16).ToInt64(); }
    static INPUT Key(ushort vk, bool up, bool ext) {
        var i = new INPUT(); i.type = 1; i.u.ki.vk = vk; i.u.ki.flags = (up ? 2u : 0u) | (ext ? 1u : 0u); return i;
    }
    // Presses the keys in order, releases them in reverse order.
    public static void Chord(ushort[] vks, bool[] ext) {
        var list = new INPUT[vks.Length * 2];
        for (int n = 0; n < vks.Length; n++) list[n] = Key(vks[n], false, ext[n]);
        for (int n = 0; n < vks.Length; n++) list[vks.Length + n] = Key(vks[vks.Length - 1 - n], true, ext[vks.Length - 1 - n]);
        SendInput((uint)list.Length, list, Marshal.SizeOf(typeof(INPUT)));
    }
}
'@
}

# Virtual keys of the chords used here (PageUp and PageDown are extended keys).
$VK = @{ Ctrl = 0x11; Alt = 0x12; Shift = 0x10; F7 = 0x76; F9 = 0x78; F10 = 0x79; F11 = 0x7A; End = 0x23; PageUp = 0x21; PageDown = 0x22 }
$EXT = @('PageUp', 'PageDown', 'End')
$script:lastInject = Get-Date
function Send-Chord([string]$text) {
    $names = $text -split '\+'
    [PhoxiumKeys]::Chord([uint16[]]($names | ForEach-Object { $VK[$_] }), [bool[]]($names | ForEach-Object { $EXT -contains $_ }))
    $script:lastInject = Get-Date
}

$idle0 = [PhoxiumKeys]::IdleSeconds()
if ($idle0 -lt $MinIdle) { Write-Output ("NOT RUN: user input {0:N0} s ago (needs {1} s idle)" -f $idle0, $MinIdle); exit 2 }

$csv = Join-Path $OutDir 'focus.csv'
$phoxiumOut = Join-Path $OutDir 'phoxium.txt'
$monitor = Join-Path $PSScriptRoot 'focus-monitor.ps1'
$started = @()
$presses = @()          # F11 presses: time of each
$fail = @()
$aborted = $false
try {
    $mon = Start-Process powershell -ArgumentList @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$monitor`"",
        '-OutFile', "`"$csv`"", '-DurationSeconds', 90) -NoNewWindow -RedirectStandardOutput (Join-Path $OutDir 'monitor.txt') -PassThru
    $started += $mon
    Start-Sleep -Seconds 3
    $demo = Start-Process (Join-Path $DemoBin 'pixel-demo.exe') -ArgumentList '--scale 4' -PassThru
    $started += $demo
    Start-Sleep -Seconds 2
    $demo.Refresh()
    $null = [PhoxiumKeys]::Activate($demo.MainWindowHandle)
    $script:lastInject = Get-Date
    Start-Sleep -Seconds 2
    $phoxiumStart = Get-Date
    $phoxium = Start-Process (Join-Path $Bin $Exe) -ArgumentList $PhoxiumArgs `
        -RedirectStandardOutput $phoxiumOut -RedirectStandardError "$phoxiumOut.err" -NoNewWindow -PassThru
    $started += $phoxium
    Start-Sleep -Seconds 5
    $phoxium.Refresh()
    $hwnd = $phoxium.MainWindowHandle
    if ($phoxium.HasExited) { $fail += "phoxium exited at startup (see $phoxiumOut.err)" }
    $out = { if (Test-Path $phoxiumOut) { Get-Content $phoxiumOut -Raw } else { '' } }
    $WS_CAPTION = 0xC00000

    # 2. registration
    $text = & $out
    $line = ($text -split "`r?`n" | Where-Object { $_ -like 'hotkeys:*' } | Select-Object -First 1)
    if (-not $line) { $fail += 'no "hotkeys:" line in the console output' }
    elseif ($line -match 'unavailable') { $fail += "a hotkey is unavailable: $line" }
    Write-Output "console: $line"
    $err = if (Test-Path "$phoxiumOut.err") { Get-Content "$phoxiumOut.err" } else { @() }
    foreach ($w in ($err | Where-Object { $_ -match 'hotkey' })) { Write-Output "registration note: $w" }

    # 3. F11 x 6, all inside the first 25 s after the start where possible.
    $expect = @('windowed', 'fullscreen', 'windowed', 'fullscreen', 'windowed', 'fullscreen')
    foreach ($want in $expect) {
        $before = @((& $out) -split "`r?`n" | Where-Object { $_ -like 'output:*' }).Count
        $presses += Get-Date
        Send-Chord 'F11'
        Start-Sleep -Milliseconds 1800
        # @(): with a single matching line PowerShell returns a string and [-1] would be its last character.
        $lines = @((& $out) -split "`r?`n" | Where-Object { $_ -like 'output:*' })
        if ($lines.Count -le $before) { $fail += "F11 -> $want : no console line"; continue }
        if ($lines[-1] -notlike "output: $want*") { $fail += "F11 -> $want : console says '$($lines[-1])'" }
        $style = [PhoxiumKeys]::Style($hwnd)
        $framed = ($style -band $WS_CAPTION) -eq $WS_CAPTION
        if ($want -eq 'windowed' -and -not $framed) { $fail += 'windowed output has no frame' }
        if ($want -eq 'fullscreen' -and $framed) { $fail += 'fullscreen output still has a frame' }
        if ([PhoxiumKeys]::IdleSeconds() -lt ((Get-Date) - $script:lastInject).TotalSeconds - 1) { $aborted = $true; break }
    }

    # 4. bypass and presets
    if (-not $aborted) {
        foreach ($step in @(
            @{ keys = 'Ctrl+Alt+F7'; pattern = 'bypass: on' },
            @{ keys = 'Ctrl+Alt+F7'; pattern = 'bypass: off' },
            @{ keys = 'Ctrl+Alt+PageDown'; pattern = 'preset next:' },
            @{ keys = 'Ctrl+Alt+PageUp'; pattern = 'preset previous:' })) {
            Send-Chord $step.keys
            Start-Sleep -Milliseconds 1200
            if (((& $out) -split "`r?`n" | Where-Object { $_ -like "*$($step.pattern)*" }).Count -lt 1) { $fail += "$($step.keys): no '$($step.pattern)' line" }
            if ([PhoxiumKeys]::IdleSeconds() -lt ((Get-Date) - $script:lastInject).TotalSeconds - 1) { $aborted = $true; break }
        }
    }

    # 5. quit
    Send-Chord 'Ctrl+Alt+End'
    $null = $phoxium.WaitForExit(3000)
    $quitOk = $phoxium.HasExited
    if (-not $quitOk) { $fail += 'Ctrl+Alt+End did not end phoxium within 3 s' }
    $phoxiumEnd = Get-Date
    Start-Sleep -Seconds 2
} finally {
    foreach ($p in @($phoxium, $demo)) { if ($p -and -not $p.HasExited) { Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue } }
    if ($mon) { $null = $mon.WaitForExit(15000); if (-not $mon.HasExited) { Stop-Process -Id $mon.Id -Force -ErrorAction SilentlyContinue } }
}

if ($aborted) { Write-Output 'ABORTED: user input during the run'; exit 3 }

# Foreground check: events for the output in the 1.5 s after each F11 press, minus the T-07 hand-backs.
$rows = Import-Csv $csv
$from = $phoxiumStart.ToString('HH:mm:ss.fff'); $to = $phoxiumEnd.ToString('HH:mm:ss.fff')
$during = $rows | Where-Object { $_.time -ge $from -and $_.time -le $to -and $_.event -ne 'end' }
$before = $rows | Where-Object { $_.time -lt $from -and $_.event -in 'baseline', 'foreground' } | Select-Object -Last 1
$fg = @($during | Where-Object { $_.event -eq 'foreground' })
$handedBack = 0; $other = 0; $i = 0
while ($i -lt $fg.Count) {
    $a = $fg[$i]; $b = if ($i + 1 -lt $fg.Count) { $fg[$i + 1] } else { $null }
    if ($a.exe -ieq $Exe -and $b -and ([int64]$b.t_ms - [int64]$a.t_ms) -le 1000) { $handedBack++; $i += 2 } else { $other++; $i++ }
}
$focusOnOutput = @($during | Where-Object { $_.exe -ieq $Exe -and $_.event -eq 'focus' }).Count
Write-Output ("foreground before: {0}; foreground changes: {1}; handed back (T-07): {2}; other: {3}; focus events on the output: {4}" -f $before.exe, $fg.Count, $handedBack, $other, $focusOnOutput)
if ($other -gt 0) { $fail += "$other foreground change(s) that are not a T-07 hand-back (see $csv)" }
if ($focusOnOutput -gt 0) { $fail += "$focusOnOutput keyboard focus event(s) on the output" }
foreach ($f in $fail) { Write-Output "FAIL: $f" }
if ($fail.Count -gt 0) { Write-Output 'FAIL'; exit 1 }
if ($handedBack -gt 0) { Write-Output 'MITIGATED (only T-07 foreground blips, handed back)'; exit 4 }
Write-Output 'PASS'
exit 0
