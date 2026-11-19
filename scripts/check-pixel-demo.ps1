<#
.SYNOPSIS
    Live check of pixel-demo as a universal test source (T-95). NOT run by the agent that wrote it.
.DESCRIPTION
    Exercises the display modes with the capture check: for each case it starts pixel-demo, verifies
    the window client size, runs `ultimatecrt --save-capture` on it, checks the capture size equals the
    client size and decodes the binary frame counter from the capture with `pixel-demo --decode`
    (same layout flags), which proves the picture is laid out where the layout code says. Then:
      - rate: `--stats` for each update rate, passes when the mean painted rate is >= 97 % of the target
        (the DWM composes at the monitor refresh rate: a 60 Hz monitor shows at most 60 distinct frames
        per second to a capture, the painted rate printed here is what pixel-demo delivers);
      - resize: the window is resized with SetWindowPos in fit mode, the capture size must follow;
      - rounded corners: with --no-round-corners the bottom-left corner pixel must be the source pixel;
      - DPI (with -MoveTo "x,y", a monitor with another scaling): the client size of an integer mode
        window must stay exactly the content size after the move (WM_DPICHANGED fix).
    Opens windows: run it under the desktop and GPU token, hands off the keyboard and mouse. Every run
    is bounded and leftovers are killed. Exit code 0 when every check passes.
.PARAMETER TargetDir
    Cargo target directory holding release\ultimatecrt.exe and release\pixel-demo.exe.
.PARAMETER Rates
    Update rates to measure (default 30,50,60,75,120). Rates above the monitor refresh are still painted.
.PARAMETER MoveTo
    "x,y": after start, move the window there (another monitor with another DPI) and check the client size.
.PARAMETER KeepFiles
    Keep the captures in the work directory.
#>
param(
    [string]$TargetDir = (Join-Path $PSScriptRoot '..\target'),
    [int[]]$Rates = @(30, 50, 60, 75, 120),
    [string]$MoveTo = "",
    [switch]$KeepFiles
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
Add-Type -AssemblyName System.Drawing

$ucrt = Join-Path $TargetDir 'release\ultimatecrt.exe'
$demo = Join-Path $TargetDir 'release\pixel-demo.exe'
if (-not (Test-Path $ucrt) -or -not (Test-Path $demo)) { throw "release binaries not found under $TargetDir" }
$work = Join-Path ([IO.Path]::GetTempPath()) ('ucrt-check-pixel-demo-' + [guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory $work | Out-Null

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class PdWin {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr c);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr a, int x, int y, int cx, int cy, uint f);
}
'@
[void][PdWin]::SetProcessDpiAwarenessContext([IntPtr](-4))   # per-monitor v2: physical pixels everywhere

$failures = @()
function Fail([string]$msg) { Write-Host "FAIL: $msg"; $script:failures += $msg }
function Pass([string]$msg) { Write-Host "ok:   $msg" }
function Stop-Leftovers { Get-Process ultimatecrt, pixel-demo -ErrorAction SilentlyContinue | Stop-Process -Force }

function Start-Demo([string[]]$DemoArgs) {
    Stop-Leftovers
    $p = Start-Process -FilePath $demo -ArgumentList $DemoArgs -PassThru
    $deadline = (Get-Date).AddSeconds(10)
    while ($p.MainWindowHandle -eq 0 -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 100; $p.Refresh() }
    Start-Sleep -Milliseconds 800
    return $p
}

function Get-Client($p) {
    $c = New-Object PdWin+RECT
    [void][PdWin]::GetClientRect($p.MainWindowHandle, [ref]$c)
    return @($c.R, $c.B)
}

function Save-Capture([string]$name) {
    $cap = Join-Path $work "$name.png"
    $u = Start-Process -FilePath $ucrt -ArgumentList @('--window', 'pixel-demo', '--save-capture', $cap, '--exit-after', '20') -PassThru -NoNewWindow `
        -RedirectStandardOutput (Join-Path $work 'u.out') -RedirectStandardError (Join-Path $work 'u.err')
    if (-not $u.WaitForExit(25000)) { $u.Kill() }
    if (Test-Path $cap) { return $cap } else { return $null }
}

function PngSize([string]$path) {
    $b = New-Object System.Drawing.Bitmap $path
    try { return @($b.Width, $b.Height) } finally { $b.Dispose() }
}

# One display-mode case. $Layout are the flags that define the layout (also given to --decode),
# $Extra other flags, $Client the expected client size.
function Test-Case([string]$Name, [string[]]$Layout, [string[]]$Extra, [int[]]$Client) {
    $p = Start-Demo ($Layout + $Extra + @('--content', 'motion', '--fixed-seed'))
    try {
        $c = Get-Client $p
        if ($c[0] -ne $Client[0] -or $c[1] -ne $Client[1]) { Fail "$Name : client $($c[0])x$($c[1]), expected $($Client[0])x$($Client[1])"; return }
        $cap = Save-Capture $Name
        if (-not $cap) { Fail "$Name : no capture saved"; return }
        $s = PngSize $cap
        if ($s[0] -ne $Client[0] -or $s[1] -ne $Client[1]) { Fail "$Name : capture $($s[0])x$($s[1]), expected the client size $($Client[0])x$($Client[1])"; return }
        $out = & $demo --decode $cap @Layout
        if ($LASTEXITCODE -ne 0) { Fail "$Name : frame counter not found ($out)"; return }
        Pass "$Name : client and capture $($s[0])x$($s[1]), $out"
    } finally { Stop-Leftovers }
}

try {
    # --- layout cases -----------------------------------------------------------------------
    Test-Case 'native-256x240'      @('--native', '256x240', '--mode', 'native')                @() @(256, 240)
    Test-Case 'integer-x3-default'  @('--native', '256x240', '--mode', 'integer', '--scale', '3') @() @(768, 720)
    Test-Case 'integer-x4-bars'     @('--native', '256x240', '--mode', 'integer', '--scale', '4') @('--window-size', '1280x1024') @(1280, 1024)
    Test-Case 'integer-x2-topleft'  @('--native', '320x240', '--mode', 'integer', '--scale', '2', '--align', 'top-left') @('--window-size', '900x700') @(900, 700)
    Test-Case 'fit-1000x800'        @('--native', '256x240', '--mode', 'fit')                   @('--window-size', '1000x800') @(1000, 800)
    Test-Case 'fit-640x480-1080p'   @('--native', '640x480', '--mode', 'fit')                   @('--window-size', '1920x1080') @(1920, 1080)
    Test-Case 'fit-free-nearest'    @('--native', '256x240', '--mode', 'fit-free')              @('--window-size', '1000x800') @(1000, 800)
    Test-Case 'fit-free-bilinear'   @('--native', '256x240', '--mode', 'fit-free')              @('--filter', 'bilinear', '--window-size', '1000x800') @(1000, 800)
    Test-Case 'fit-free-bicubic'    @('--native', '256x240', '--mode', 'fit-free')              @('--filter', 'bicubic', '--window-size', '1000x800') @(1000, 800)
    Test-Case 'par-8-7-integer-x3'  @('--native', '256x240', '--mode', 'integer', '--scale', '3', '--par', '8:7') @() @(877, 720)
    Test-Case 'native-1280x720'     @('--native', '1280x720', '--mode', 'native')               @() @(1280, 720)
    Test-Case 'borderless-x2'       @('--native', '256x240', '--mode', 'integer', '--scale', '2') @('--borderless') @(512, 480)
    Write-Host 'note: fit-free bilinear/bicubic are the smoothed-source cases: run ultimatecrt --window pixel-demo on them by hand and look for the smoothed-source warning.'

    # --- resize re-layout ------------------------------------------------------------------
    $p = Start-Demo @('--native', '256x240', '--mode', 'fit', '--content', 'motion', '--fixed-seed', '--window-size', '800x600')
    try {
        $o = New-Object PdWin+RECT; $c = New-Object PdWin+RECT
        [void][PdWin]::GetWindowRect($p.MainWindowHandle, [ref]$o); [void][PdWin]::GetClientRect($p.MainWindowHandle, [ref]$c)
        $nw = ($o.R - $o.L) + (1100 - $c.R); $nh = ($o.B - $o.T) + (700 - $c.B)
        [void][PdWin]::SetWindowPos($p.MainWindowHandle, [IntPtr]::Zero, 0, 0, $nw, $nh, 0x0002 -bor 0x0004 -bor 0x0010)
        Start-Sleep -Milliseconds 800
        $cap = Save-Capture 'resize'
        if (-not $cap) { Fail 'resize : no capture saved' }
        else {
            $s = PngSize $cap
            $out = & $demo --decode $cap --native 256x240 --mode fit
            if ($s[0] -eq 1100 -and $s[1] -eq 700 -and $LASTEXITCODE -eq 0) { Pass "resize : capture $($s[0])x$($s[1]) re-laid out, $out" }
            else { Fail "resize : capture $($s[0])x$($s[1]) (expected 1100x700), decode exit $LASTEXITCODE" }
        }
    } finally { Stop-Leftovers }

    # --- rounded corners -------------------------------------------------------------------
    foreach ($flag in @('--no-round-corners')) {
        $p = Start-Demo @('--native', '256x240', '--mode', 'native', '--content', 'bars', $flag)
        try {
            $cap = Save-Capture 'corners'
            if (-not $cap) { Fail 'corners : no capture saved' }
            else {
                $b = New-Object System.Drawing.Bitmap $cap
                try {
                    $px = $b.GetPixel(0, $b.Height - 1)   # bottom-left of the 50 % white bar: 127,127,127
                    if ($px.R -eq 127 -and $px.G -eq 127 -and $px.B -eq 127) { Pass "corners : bottom-left pixel intact with $flag" }
                    else { Fail "corners : bottom-left pixel is $($px.R),$($px.G),$($px.B), expected 127,127,127 (rounded corner still darkens it)" }
                } finally { $b.Dispose() }
            }
        } finally { Stop-Leftovers }
    }

    # --- DPI change ------------------------------------------------------------------------
    if ($MoveTo) {
        $p = Start-Demo @('--native', '256x240', '--mode', 'integer', '--scale', '3', '--content', 'motion')
        try {
            $xy = $MoveTo.Split(',')
            [void][PdWin]::SetWindowPos($p.MainWindowHandle, [IntPtr]::Zero, [int]$xy[0], [int]$xy[1], 0, 0, 0x0001 -bor 0x0004 -bor 0x0010)
            Start-Sleep -Milliseconds 1200
            $c = Get-Client $p
            $dpi = [PdWin]::GetDpiForWindow($p.MainWindowHandle)
            if ($c[0] -eq 768 -and $c[1] -eq 720) { Pass "dpi : client stays 768x720 after the move (window dpi $dpi)" }
            else { Fail "dpi : client $($c[0])x$($c[1]) after the move (window dpi $dpi), expected 768x720" }
        } finally { Stop-Leftovers }
    } else {
        Write-Host 'skipped: DPI check (give -MoveTo "x,y" of a monitor with another scaling)'
    }

    # --- update rates ----------------------------------------------------------------------
    foreach ($fps in $Rates) {
        Stop-Leftovers
        $out = Join-Path $work "rate-$fps.txt"
        $p = Start-Process -FilePath $demo -ArgumentList @('--native', '256x240', '--mode', 'native', '--content', 'motion', '--fps', $fps, '--stats', '--exit-after', '6') `
            -PassThru -RedirectStandardOutput $out
        if (-not $p.WaitForExit(20000)) { $p.Kill() }
        $painted = @(Get-Content $out | Select-String 'painted ([0-9.]+)/s' | ForEach-Object { [double]$_.Matches[0].Groups[1].Value })
        if ($painted.Count -lt 3) { Fail "rate $fps Hz : no stats lines"; continue }
        $mean = ($painted | Measure-Object -Average).Average
        $line = "rate $fps Hz : painted mean {0:N1}/s over {1} s" -f $mean, $painted.Count
        if ($mean -ge 0.97 * $fps) { Pass $line } else { Fail "$line (below 97 % of the target)" }
    }
} finally {
    Stop-Leftovers
    if (-not $KeepFiles) { Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue } else { Write-Host "files kept in $work" }
}

if ($failures.Count -eq 0) { Write-Host 'PASS: every check passed'; exit 0 }
else { Write-Host "FAIL: $($failures.Count) check(s) failed"; exit 1 }
