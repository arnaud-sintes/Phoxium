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
      6. stop everything; judge the independent poll cycle by cycle (Test-MenuForeground, not a count of all the
         changes): menu open => the output has the foreground within the settle time and keeps it until the
         close; menu closed => the source has the foreground within the settle time and keeps it, except T-07
         hand-back blips (output for at most 1 s). The state at the start of the poll is not judged (the
         startup hand-back of fullscreen happens before the poll starts, and a windowed output may still hold
         the foreground at the start: both are reported as notes); zero validation messages in the ultimatecrt
         error output.
      -SelfTest runs the verdict function on recorded sample sequences (no window, no build, no input).
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
.PARAMETER SelfTest
    Test the pass criterion on recorded foreground sequences (the two false FAILs of the 2026-11-19 live run, a
    correct run, broken runs) and exit 0 (all as expected) or 1. Opens nothing.
#>
param(
    [string]$Bin = '',
    [int]$Cycles = 10,
    [int]$ClosedSeconds = 60,
    [int]$MenuKey = 0x79,
    [string[]]$Modes = @('windowed', 'fullscreen'),
    [string]$OutDir = (Join-Path $env:TEMP 'ultimatecrt-check-menu'),
    [int]$MinIdle = 60,
    [switch]$NoBuild,
    [switch]$SelfTest
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$root = Split-Path $PSScriptRoot -Parent
# --- Pass criterion (pure: no Win32, no files; testable with -SelfTest) ---------------------------------------
# A foreground sample is @{ Time = [DateTime]; State = 'source' | 'output' | 'other' }: one per CHANGE of the
# foreground window (the rows of poll-foreground.ps1, the first row is the state when the poll started).
# A cycle is @{ Open = [DateTime]; Close = [DateTime] }: when the script sent the menu key to open and to close.

# State of the foreground at time $t: the last sample at or before it (the first sample before the poll started).
function Get-ForegroundStateAt($Samples, [DateTime]$t) {
    $state = $Samples[0].State
    foreach ($s in $Samples) { if ($s.Time -le $t) { $state = $s.State } else { break } }
    return $state
}

# The first time in [$from, $to] at which the foreground is in $state: $from itself when it already is, else the
# first sample change into it; $null when it never is.
function Find-ForegroundState($Samples, [string]$State, [DateTime]$from, [DateTime]$to) {
    if ((Get-ForegroundStateAt $Samples $from) -eq $State) { return $from }
    foreach ($s in $Samples) {
        if ($s.Time -gt $from -and $s.Time -le $to -and $s.State -eq $State) { return $s.Time }
    }
    return $null
}
# The runs of constant state inside [$from, $to): @{ State; Start; End } (clipped to the window).
function Get-ForegroundRuns($Samples, [DateTime]$from, [DateTime]$to) {
    $runs = @()
    if ($to -le $from) { return $runs }
    $cursor = $from
    $state = Get-ForegroundStateAt $Samples $from
    foreach ($s in $Samples) {
        if ($s.Time -le $from) { continue }
        if ($s.Time -ge $to) { break }
        if ($s.State -ne $state) {
            $runs += @{ State = $state; Start = $cursor; End = $s.Time }
            $cursor = $s.Time
            $state = $s.State
        }
    }
    $runs += @{ State = $state; Start = $cursor; End = $to }
    return $runs
}

# Judges a menu run. Returns @{ Pass; Reasons; Notes; Blips; Opens; Closes }.
#  - Open: within SettleMs of the open key the output has the foreground, and keeps it until the close key (the
#    menu does not lose the focus while it is open).
#  - Close: within SettleMs of the close key the source has the foreground, and while the menu stays closed (until
#    the next open key, or PollEnd) only the source holds it, except a T-07 hand-back blip (output for at most
#    BlipMs). SettleMs is capped at half of the interval it applies to.
#  - Before the first Open, after the startup grace: the same closed-menu rule, except that the output holding
#    the foreground from the very start of the poll is accepted as a startup state (noted, not a failure):
#    fullscreen has its startup hand-back before the poll starts (at 1 s, the poll at 3 s) and a windowed
#    output may still have the foreground when the poll starts (2026-11-19 live run, where counting every
#    change gave 19 instead of 20 and 20 instead of 22 although the menu worked).
#  - Any other window holding the foreground (outside the startup state) is a failure.
function Test-MenuForeground($Samples, $Cycles, [DateTime]$PollEnd, [int]$SettleMs = 1500, [int]$BlipMs = 1000) {
    $reasons = New-Object System.Collections.ArrayList
    $notes = New-Object System.Collections.ArrayList
    $blips = 0
    $opens = 0
    $closes = 0
    $samples = @($Samples)
    if ($samples.Count -eq 0 -or @($Cycles).Count -eq 0) {
        [void]$reasons.Add('no foreground sample or no cycle to judge')
        return @{ Pass = $false; Reasons = @($reasons); Notes = @(); Blips = 0; Opens = 0; Closes = 0 }
    }
    $settle = [TimeSpan]::FromMilliseconds($SettleMs)
    $blip = [TimeSpan]::FromMilliseconds($BlipMs)
    $cycles = @($Cycles)

    # Closed-menu rule over [from, to): only the source, except blips; $leading = the first run may be the
    # startup state (the output from the start of the poll).
    $checkClosed = {
        param([DateTime]$from, [DateTime]$to, [string]$what, [bool]$leading)
        $first = $true
        foreach ($r in (Get-ForegroundRuns $samples $from $to)) {
            $len = $r.End - $r.Start
            if ($r.State -eq 'source') { $first = $false; continue }
            if ($r.State -eq 'output') {
                if ($first -and $leading) {
                    [void]$notes.Add(('startup: the output held the foreground at the start of the poll for {0:N1} s (before the first menu open)' -f $len.TotalSeconds))
                } elseif ($len -le $blip) {
                    $script:blipCount++
                } else {
                    [void]$reasons.Add(('{0}: the output kept the foreground for {1:N1} s while the menu was closed (T-07 hand-back blips last at most {2} ms)' -f $what, $len.TotalSeconds, $BlipMs))
                }
            } else {
                [void]$reasons.Add(('{0}: another window held the foreground for {1:N1} s while the menu was closed' -f $what, $len.TotalSeconds))
            }
            $first = $false
        }
    }
    $script:blipCount = 0

    $firstOpen = $cycles[0].Open
    $pollStart = $samples[0].Time
    # Startup: the state at the first open must be the source or the output, then judge the closed time.
    $before = Get-ForegroundStateAt $samples $firstOpen
    if ($before -eq 'other') { [void]$reasons.Add('first open: another window held the foreground just before the menu key') }
    $closedFrom = $pollStart + $settle
    if ($closedFrom -lt $firstOpen) { & $checkClosed $closedFrom $firstOpen 'before the first open' $true }

    for ($i = 0; $i -lt $cycles.Count; $i++) {
        $c = $cycles[$i]
        $n = $i + 1
        $next = $PollEnd
        if ($i + 1 -lt $cycles.Count) { $next = $cycles[$i + 1].Open }
        # The settle time is capped at half of the interval it applies to (the open hold is about 1.2 s, the gap
        # after a close about 0.8 s in the real run).
        $openSettle = [TimeSpan]::FromTicks([Math]::Min($settle.Ticks, ($c.Close - $c.Open).Ticks / 2))
        $closeSettle = [TimeSpan]::FromTicks([Math]::Min($settle.Ticks, ($next - $c.Close).Ticks / 2))
        # Open: the output gets the foreground within the settle time and keeps it until the close.
        $reached = Find-ForegroundState $samples 'output' $c.Open ($c.Open + $openSettle)
        if ($null -ne $reached) {
            $opens++
            foreach ($r in (Get-ForegroundRuns $samples $reached $c.Close)) {
                if ($r.State -ne 'output') {
                    [void]$reasons.Add(('cycle {0}: the menu lost the foreground to the {1} while it was open ({2:N1} s)' -f $n, $r.State, ($r.End - $r.Start).TotalSeconds))
                    break
                }
            }
        } else {
            $now = Get-ForegroundStateAt $samples ($c.Open + $openSettle)
            [void]$reasons.Add(('cycle {0}: no output within {1:N1} s after the open key (the foreground is the {2})' -f $n, $openSettle.TotalSeconds, $now))
        }
        # Close: the source gets the foreground within the settle time and keeps it (blips apart) until the next open.
        $reached = Find-ForegroundState $samples 'source' $c.Close ($c.Close + $closeSettle)
        if ($null -ne $reached) {
            $closes++
            & $checkClosed $reached $next ("after close $n") $false
        } else {
            $now = Get-ForegroundStateAt $samples ($c.Close + $closeSettle)
            [void]$reasons.Add(('cycle {0}: no source within {1:N1} s after the close key (the foreground is the {2})' -f $n, $closeSettle.TotalSeconds, $now))
        }
    }
    $blips = $script:blipCount
    return @{ Pass = ($reasons.Count -eq 0); Reasons = @($reasons); Notes = @($notes); Blips = $blips; Opens = $opens; Closes = $closes }
}

# Foreground samples from a poll-foreground.ps1 CSV: the source flag, else a title starting with 'ultimatecrt'
# is the output, anything else is another window.
function Read-ForegroundSamples([string]$Path) {
    $samples = @()
    foreach ($row in (Import-Csv -LiteralPath $Path)) {
        $state = 'other'
        if ($row.is_source -eq '1') { $state = 'source' } elseif ($row.title -like 'ultimatecrt*') { $state = 'output' }
        $t = [DateTime]::ParseExact($row.timestamp, 'yyyy-MM-ddTHH:mm:ss.fff', [Globalization.CultureInfo]::InvariantCulture)
        $samples += @{ Time = $t; State = $state }
    }
    return $samples
}

# Builds a recorded run: $Start is the state at the poll start, then per cycle (open at 10 + 3 i s, close 1.5 s
# later) the foreground changes the real run produced, minus the ones in $Skip ('open3', 'close4', ...), plus
# the extra samples in $Extra (strings "seconds:state", e.g. "5.0:output"). Returns @{ Samples; Cycles; PollEnd; Changes }.
function New-MenuRun([string]$Start, [int]$Cycles = 10, [string[]]$Skip = @(), [string[]]$Extra = @()) {
    $t0 = [DateTime]'2026-11-19T12:00:00'
    $samples = @(@{ Time = $t0; State = $Start })
    $cyc = @()
    for ($i = 1; $i -le $Cycles; $i++) {
        $open = 10.0 + 3 * ($i - 1)
        $close = $open + 1.5
        $cyc += @{ Open = $t0.AddSeconds($open); Close = $t0.AddSeconds($close) }
        if ($Skip -notcontains "open$i") { $samples += @{ Time = $t0.AddSeconds($open + 0.08); State = 'output' } }
        if ($Skip -notcontains "close$i") { $samples += @{ Time = $t0.AddSeconds($close + 0.08); State = 'source' } }
    }
    foreach ($e in $Extra) { $p = $e -split ':'; $samples += @{ Time = $t0.AddSeconds([double]$p[0]); State = $p[1] } }
    $samples = @($samples | Sort-Object { $_.Time })
    # Consecutive duplicates are not changes: the poll never writes them.
    $dedup = @($samples[0])
    for ($i = 1; $i -lt $samples.Count; $i++) { if ($samples[$i].State -ne $dedup[-1].State) { $dedup += $samples[$i] } }
    return @{ Samples = $dedup; Cycles = $cyc; PollEnd = $t0.AddSeconds(10 + 3 * $Cycles + 3); Changes = $dedup.Count - 1 }
}

if ($SelfTest) {
    $cases = @(
        # name, run, expected pass, expected fragment of a reason (or of a note for passing runs), blips
        @('correct windowed run (source at the start): 20 changes', (New-MenuRun 'source'), $true, '', 0),
        @('false FAIL 1, windowed: output already had the foreground before the first open (19 changes, not 20)', (New-MenuRun 'output' -Skip @('open1')), $true, 'startup', 0),
        @('false FAIL 2, fullscreen: startup hand-back before the poll (20 changes, not 22)', (New-MenuRun 'source'), $true, '', 0),
        @('correct run with a T-07 blip while closed (22 changes)', (New-MenuRun 'source' -Extra @('5.0:output', '5.4:source')), $true, '', 1),
        @('broken: close 4 never gives the foreground back to the source', (New-MenuRun 'source' -Skip @('close4')), $false, 'cycle 4: no source within 0.8 s after the close key', 0),
        @('broken: open 2 leaves the foreground on the source', (New-MenuRun 'source' -Skip @('open2')), $false, 'cycle 2: no output within 0.8 s after the open key', 0),
        @('broken: the menu loses the foreground by itself while open (cycle 3)', (New-MenuRun 'source' -Extra @('16.9:source', '17.1:output')), $false, 'cycle 3: the menu lost the foreground', 0),
        @('broken: another window takes the foreground while the menu is closed', (New-MenuRun 'source' -Extra @('8.0:other', '9.0:source')), $false, 'another window held the foreground', 0),
        @('broken: the output keeps the foreground for 4 s while the menu is closed', (New-MenuRun 'source' -Cycles 2 -Extra @('15.0:output')), $false, 'kept the foreground for 4.0 s', 0),
        @('broken: another window holds the foreground at the first open', (New-MenuRun 'other' -Skip @('open1')), $false, 'first open', 0)
    )
    $bad = 0
    foreach ($c in $cases) {
        $run = $c[1]
        $v = Test-MenuForeground $run.Samples $run.Cycles $run.PollEnd
        $ok = ($v.Pass -eq $c[2]) -and ($v.Blips -eq $c[4])
        $text = (@($v.Reasons) + @($v.Notes)) -join ' | '
        if ($c[3] -ne '' -and -not $text.Contains($c[3])) { $ok = $false }
        $label = 'FAIL'; if ($ok) { $label = 'ok  ' }
        Write-Host ("  {0} {1} (changes {2}, pass {3}, blips {4}, opens {5}, closes {6})" -f $label, $c[0], $run.Changes, $v.Pass, $v.Blips, $v.Opens, $v.Closes)
        if (-not $ok) { $bad++; Write-Host ("       reasons: {0}" -f $text) }
    }
    if ($bad -eq 0) { Write-Host "check-menu SelfTest: PASS ($($cases.Count) sequences)"; exit 0 }
    Write-Host "check-menu SelfTest: FAIL ($bad of $($cases.Count))"
    exit 1
}
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
    # -NoNewWindow: without it Windows opens a console window (Windows Terminal) for ultimatecrt that takes
    # the foreground and becomes the menu's restore target (seen live 2026-11-19).
    $up = Start-Process -FilePath $ucrt -ArgumentList $ucrtArgs -PassThru -NoNewWindow -RedirectStandardOutput $outLog -RedirectStandardError $errLog
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

    $opens = 0; $closes = 0; $cycleTimes = @()
    for ($c = 1; $c -le $Cycles; $c++) {
        $tOpen = [DateTime]::Now
        [UcrtMenuCheck]::Tap([uint16]$MenuKey)
        $t = Wait-Foreground 'ultimatecrt*' 1000
        if ($t -like 'ultimatecrt*') { $opens++ } else { $reasons += "cycle ${c}: open left the foreground on '$t'" }
        Start-Sleep -Milliseconds 300
        [UcrtMenuCheck]::Type('t52')
        Start-Sleep -Milliseconds 700
        $tClose = [DateTime]::Now
        [UcrtMenuCheck]::Tap([uint16]$MenuKey)
        $t = Wait-Foreground '*pixel-demo*' 1000
        if ($t -like '*pixel-demo*') { $closes++ } else { $reasons += "cycle ${c}: close left the foreground on '$t'" }
        Start-Sleep -Milliseconds 800
        $cycleTimes += @{ Open = $tOpen; Close = $tClose }
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
    # Pass criterion: the open and close cycles, one by one, against the independent poll (Test-MenuForeground).
    # The total number of changes is only printed: it depends on the state when the poll starts (startup
    # hand-back, output still holding the foreground) and on the hand-back blips.
    $m = [regex]::Match($pollOut, 'Foreground changes: (\d+)')
    if ($m.Success) { Write-Host "  independent poll: $($m.Groups[1].Value) foreground changes (informational)" }
    $pollEnd = [DateTime]::Now
    if (Test-Path $pollCsv) {
        try {
            $samples = @(Read-ForegroundSamples $pollCsv)
            $v = Test-MenuForeground $samples $cycleTimes $pollEnd
            Write-Host "  independent poll, per cycle: opens to the output $($v.Opens) / $Cycles, closes to the source $($v.Closes) / $Cycles, hand-back blips while closed $($v.Blips)"
            foreach ($n in $v.Notes) { Write-Host "  note: $n" }
            foreach ($r in $v.Reasons) { $reasons += "independent poll: $r" }
        } catch { $reasons += "independent poll unreadable: $($_.Exception.Message)" }
    } else { $reasons += 'independent poll gave no result' }    if (Test-Path $errLog) {
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
