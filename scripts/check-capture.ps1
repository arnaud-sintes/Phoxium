<#
.SYNOPSIS
    Capture fidelity check (T-12, SP-5, QUESTIONS CAP-3): are captured pixels bit-exact?
.DESCRIPTION
    Generates a known PNG (grey and colour ramps plus seeded random noise, 256x240), starts pixel-demo
    showing it with --image at each integer scale, runs `ultimatecrt --save-capture`, then compares the
    saved capture with the known pixels: the capture must be (width*scale) x (height*scale) (or the native
    size) and every block of scale x scale pixels must equal the source pixel. The two bottom 10x10 pixel corners are reported apart: Windows 11 rounds the window corners and darkens those pixels (36 pixels measured). Prints the max channel
    difference and the number of differing pixels per condition. Exit code 0 when every condition is exact.
    -Occlude additionally covers the left half of the source with a topmost non-activating form for the run.
    Opens windows: run it under the desktop and GPU token, hands off the keyboard and mouse. Each run is
    bounded (--save-capture exits after one frame, hard timeout, leftovers killed).
.PARAMETER TargetDir
    Cargo target directory holding release\ultimatecrt.exe and release\pixel-demo.exe. Default: CARGO_TARGET_DIR,
    else the target directory cargo metadata reports. Binaries older than their sources are refused (rebuild with
    cargo build --release --locked).
.PARAMETER Scales
    Integer scales to test (default 1,2,3,4; 5 does not fit a 1080p screen: 1200 px tall).
.PARAMETER Occlude
    Also run each scale with the left half of the source covered by another window.
.PARAMETER MoveTo
    "x,y": move the pixel-demo window there (virtual-desktop pixels, e.g. onto another monitor with another DPI)
    before capturing. The window is only moved, never resized. The client area is then resized to exactly width*scale x height*scale (the non-client area scales with the DPI; pixel-demo now handles WM_DPICHANGED and keeps an exact-size window exact, this resize stays as a safety net). Reported DPI is that of the window.
.PARAMETER KeepFiles
    Keep the generated PNGs in the work directory.
#>
param(
    [string]$TargetDir = '',
    [int[]]$Scales = @(1, 2, 3, 4),
    [switch]$Occlude,
    [string]$MoveTo = "",
    [switch]$KeepFiles
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
Add-Type -AssemblyName System.Drawing

. (Join-Path $PSScriptRoot 'build-checks.ps1')
$repoRoot = Split-Path $PSScriptRoot -Parent
if (-not $TargetDir) { $TargetDir = Get-BinaryTargetDir $repoRoot }
$ucrt = Join-Path $TargetDir 'release\ultimatecrt.exe'
$demo = Join-Path $TargetDir 'release\pixel-demo.exe'
# D-10: the binaries of this tree, not an old build elsewhere.
Assert-FreshBinaries $repoRoot @($ucrt, $demo)
# Start-Process joins -ArgumentList unquoted in Windows PowerShell 5.1: every argument is quoted (paths with spaces).
function Join-Args([object[]]$a) { return @($a | ForEach-Object { ConvertTo-ProcessArgument ([string]$_) }) }
$work = Join-Path ([IO.Path]::GetTempPath()) ('ucrt-check-capture-' + [guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory $work | Out-Null

Add-Type -ReferencedAssemblies System.Drawing -TypeDefinition @'
using System;
using System.Drawing;
using System.Drawing.Imaging;
public static class CapCmp {
    public const int W = 256, H = 240;
    public static byte[] Known() {
        var p = new byte[W * H * 3];
        var rnd = new Random(12345);
        for (int y = 0; y < H; y++) for (int x = 0; x < W; x++) {
            int r, g, b;
            if (y < 16) { r = g = b = x; }
            else if (y < 32) { r = x; g = 0; b = 0; }
            else if (y < 48) { r = 0; g = x; b = 0; }
            else if (y < 64) { r = 0; g = 0; b = x; }
            else if (y < 80) { r = g = b = ((x + y) & 1) * 255; }
            else { r = rnd.Next(256); g = rnd.Next(256); b = rnd.Next(256); }
            int i = (y * W + x) * 3; p[i] = (byte)r; p[i + 1] = (byte)g; p[i + 2] = (byte)b;
        }
        return p;
    }
    public static void WritePng(string path) {
        var k = Known();
        using (var bmp = new Bitmap(W, H, PixelFormat.Format24bppRgb)) {
            for (int y = 0; y < H; y++) for (int x = 0; x < W; x++) {
                int i = (y * W + x) * 3;
                bmp.SetPixel(x, y, Color.FromArgb(k[i], k[i + 1], k[i + 2]));
            }
            bmp.Save(path, ImageFormat.Png);
        }
    }
    static byte[] Load(string path, out int w, out int h) {
        using (var bmp = new Bitmap(path)) {
            w = bmp.Width; h = bmp.Height;
            var o = new byte[w * h * 3];
            var d = bmp.LockBits(new Rectangle(0, 0, w, h), ImageLockMode.ReadOnly, PixelFormat.Format32bppArgb);
            var row = new byte[Math.Abs(d.Stride)];
            for (int y = 0; y < h; y++) {
                System.Runtime.InteropServices.Marshal.Copy(IntPtr.Add(d.Scan0, y * d.Stride), row, 0, w * 4);
                for (int x = 0; x < w; x++) { int i = (y * w + x) * 3; o[i] = row[x * 4 + 2]; o[i + 1] = row[x * 4 + 1]; o[i + 2] = row[x * 4]; }
            }
            bmp.UnlockBits(d);
            return o;
        }
    }
    // Returns "size WxH scale N maxdiff D differing P of T" or an error text. Sets exact.
    public static string Compare(string capture, int scale, out bool exact) {
        int w, h; var c = Load(capture, out w, out h); var k = Known();
        exact = false;
        int s;
        if (w == W * scale && h == H * scale) s = scale;
        else if (w == W && h == H) s = 1;
        else return "unexpected capture size " + w + "x" + h;
        int maxd = 0; long bad = 0, corner = 0;
        for (int y = 0; y < h; y++) for (int x = 0; x < w; x++) {
            int ci = (y * w + x) * 3, ki = ((y / s) * W + (x / s)) * 3; bool diff = false;
            bool inCorner = y >= h - 10 && (x < 10 || x >= w - 10);
            for (int q = 0; q < 3; q++) { int d = Math.Abs(c[ci + q] - k[ki + q]); if (d != 0) diff = true; if (d != 0 && !inCorner && d > maxd) maxd = d; }
            if (diff) { if (inCorner) corner++; else bad++; }
        }
        exact = (bad == 0);
        return "size " + w + "x" + h + " block " + s + " maxdiff " + maxd + " differing " + bad + " of " + (long)w * h + " (plus " + corner + " pixels in the two bottom 10x10 corners, Windows 11 rounded window corners)";
    }
}
'@

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class CapWin {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr c);
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr a, int x, int y, int cx, int cy, uint f);
}
'@

[void][CapWin]::SetProcessDpiAwarenessContext([IntPtr](-4))   # per-monitor v2: physical pixels in every Win32 call below
$png = Join-Path $work 'known.png'
[CapCmp]::WritePng($png)

$occluderScript = Join-Path $work 'occluder.ps1'
@'
param($x,$y,$w,$h,$secs)
Add-Type -AssemblyName System.Windows.Forms
Add-Type -TypeDefinition @"
using System; using System.Windows.Forms;
public class NoActForm : Form { protected override bool ShowWithoutActivation { get { return true; } }
  protected override CreateParams CreateParams { get { var p = base.CreateParams; p.ExStyle |= 0x08000000 | 0x80; return p; } } }
"@ -ReferencedAssemblies System.Windows.Forms
$f = New-Object NoActForm
$f.Text = 'occluder'; $f.FormBorderStyle = 'None'; $f.StartPosition = 'Manual'; $f.TopMost = $true
$f.BackColor = [System.Drawing.Color]::Magenta
$f.Location = New-Object System.Drawing.Point($x,$y); $f.Size = New-Object System.Drawing.Size($w,$h)
$f.ShowInTaskbar = $false
$t = New-Object System.Windows.Forms.Timer; $t.Interval = $secs*1000; $t.Add_Tick({ $f.Close() }); $t.Start()
[System.Windows.Forms.Application]::Run($f)
'@ | Set-Content $occluderScript

function Stop-Leftovers {
    Get-Process ultimatecrt, pixel-demo -ErrorAction SilentlyContinue | Stop-Process -Force
}

$results = @()
$allExact = $true
try {
    foreach ($occ in @($false, $true)) {
        if ($occ -and -not $Occlude) { continue }
        foreach ($scale in $Scales) {
            Stop-Leftovers
            $cap = Join-Path $work ("cap-x$scale" + $(if ($occ) { '-occluded' } else { '' }) + '.png')
            $p = Start-Process -FilePath $demo -ArgumentList (Join-Args @('--image', $png, '--scene', 'image', '--scale', $scale)) -PassThru
            $deadline = (Get-Date).AddSeconds(10)
            while ($p.MainWindowHandle -eq 0 -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 100; $p.Refresh() }
            Start-Sleep -Milliseconds 500
            if ($MoveTo) { $xy = $MoveTo.Split(","); [void][CapWin]::SetWindowPos($p.MainWindowHandle, [IntPtr]::Zero, [int]$xy[0], [int]$xy[1], 0, 0, 0x0001 -bor 0x0004 -bor 0x0010); Start-Sleep -Milliseconds 700
                $o = New-Object CapWin+RECT; $c = New-Object CapWin+RECT; [void][CapWin]::GetWindowRect($p.MainWindowHandle, [ref]$o); [void][CapWin]::GetClientRect($p.MainWindowHandle, [ref]$c)
                $nw = ($o.R - $o.L) + (256 * $scale - $c.R); $nh = ($o.B - $o.T) + (240 * $scale - $c.B)
                [void][CapWin]::SetWindowPos($p.MainWindowHandle, [IntPtr]::Zero, 0, 0, $nw, $nh, 0x0002 -bor 0x0004 -bor 0x0010); Start-Sleep -Milliseconds 700 }
            $dpi = [CapWin]::GetDpiForWindow($p.MainWindowHandle)
            $rc = New-Object CapWin+RECT
            [void][CapWin]::GetWindowRect($p.MainWindowHandle, [ref]$rc)
            $occProc = $null
            if ($occ) {
                $ow = [int](($rc.R - $rc.L) / 2); $oh = $rc.B - $rc.T
                $occProc = Start-Process powershell -ArgumentList (Join-Args @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $occluderScript, $rc.L, $rc.T, $ow, $oh, 12)) -PassThru -WindowStyle Hidden
                Start-Sleep -Seconds 3
            }
            $u = Start-Process -FilePath $ucrt -ArgumentList (Join-Args @('--window', 'pixel-demo', '--save-capture', $cap, '--exit-after', '20')) -PassThru -NoNewWindow -RedirectStandardOutput (Join-Path $work 'u.out') -RedirectStandardError (Join-Path $work 'u.err')
            if (-not $u.WaitForExit(25000)) { $u.Kill() }
            if ($occProc -and -not $occProc.HasExited) { $occProc.Kill() }
            Stop-Leftovers
            $cond = "scale $scale$(if ($occ) { ' occluded (left half)' } else { ' windowed' }), dpi $dpi"
            if (Test-Path $cap) {
                $exact = $false
                $text = [CapCmp]::Compare($cap, $scale, [ref]$exact)
                if (-not $exact) { $allExact = $false }
                $line = "$cond : $text : " + $(if ($exact) { 'EXACT' } else { 'DIFFERS' })
            } else {
                $allExact = $false
                $line = "$cond : no capture saved (exit $($u.ExitCode)): " + ((Get-Content (Join-Path $work 'u.err') -ErrorAction SilentlyContinue) -join ' ')
            }
            Write-Host $line
            $results += $line
        }
    }
} finally {
    Stop-Leftovers
    if (-not $KeepFiles) { Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue } else { Write-Host "files kept in $work" }
}
if ($allExact) { Write-Host 'PASS: every tested condition is bit-exact'; exit 0 } else { Write-Host 'FAIL: see lines above'; exit 1 }





