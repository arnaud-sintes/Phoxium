<#
.SYNOPSIS
    Logs foreground-window changes to a CSV (poll, no hook). Used by the focus checks (T-06, T-08, T-52a).
.DESCRIPTION
    Polls GetForegroundWindow every -IntervalMs and writes one CSV row on each change (plus a baseline row
    at start): timestamp (ISO, ms), title, pid, executable file name, is_source. The CSV is flushed after
    each row. At the end (duration reached or Ctrl+C) a summary is printed: number of changes and the
    share of time the source window was in the foreground.
    Window titles can hold private content: delete the CSV after the run.
.PARAMETER OutFile
    CSV path. Default .\poll-foreground.csv (overwritten).
.PARAMETER SourceTitle
    Case-insensitive substring of the source window title. Empty = no source (is_source always 0).
.PARAMETER DurationSeconds
    Run time in seconds. 0 (default) = until Ctrl+C.
.PARAMETER IntervalMs
    Poll interval in milliseconds. Default 100.
.PARAMETER SelfTest
    Run 3 s against the current foreground window (no UI), check that the CSV holds exactly the baseline
    row, print PASS or FAIL.
#>
param(
    [string]$OutFile = '.\poll-foreground.csv',
    [string]$SourceTitle = '',
    [int]$DurationSeconds = 0,
    [int]$IntervalMs = 100,
    [switch]$SelfTest
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

if (-not ('UcrtFg' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class UcrtFg {
    [DllImport("user32.dll")] static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll")] static extern int GetWindowTextLength(IntPtr h);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("kernel32.dll")] static extern IntPtr OpenProcess(uint access, bool inherit, uint pid);
    [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr h);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode)]
    static extern bool QueryFullProcessImageName(IntPtr h, uint flags, StringBuilder s, ref int n);

    // Never throws; unknown parts stay empty or 0.
    public static void Query(out long hwnd, out uint pid, out string title, out string exe) {
        hwnd = 0; pid = 0; title = ""; exe = "";
        IntPtr h = GetForegroundWindow();
        hwnd = h.ToInt64();
        if (h == IntPtr.Zero) return;
        try {
            int len = GetWindowTextLength(h);
            StringBuilder sb = new StringBuilder(Math.Max(len, 0) + 2);
            GetWindowText(h, sb, sb.Capacity);
            title = sb.ToString();
        } catch { }
        try {
            GetWindowThreadProcessId(h, out pid);
            if (pid != 0) {
                IntPtr p = OpenProcess(0x1000, false, pid); // PROCESS_QUERY_LIMITED_INFORMATION
                if (p != IntPtr.Zero) {
                    try {
                        StringBuilder path = new StringBuilder(1024);
                        int n = path.Capacity;
                        if (QueryFullProcessImageName(p, 0, path, ref n)) {
                            exe = System.IO.Path.GetFileName(path.ToString());
                        }
                    } finally { CloseHandle(p); }
                }
            }
        } catch { }
    }
}
'@
}

function ConvertTo-CsvField([string]$s) {
    if ($null -eq $s) { $s = '' }
    $s = $s -replace '[\r\n]+', ' '
    return '"' + ($s -replace '"', '""') + '"'
}

function Invoke-Poll([string]$Path, [string]$Source, [int]$Seconds, [int]$Interval) {
    $full = $Path
    if (-not [System.IO.Path]::IsPathRooted($full)) { $full = Join-Path (Get-Location).Path $full }
    $full = [System.IO.Path]::GetFullPath($full)
    $fs = New-Object System.IO.FileStream($full, [System.IO.FileMode]::Create, [System.IO.FileAccess]::Write, [System.IO.FileShare]::ReadWrite)
    $w = New-Object System.IO.StreamWriter($fs, (New-Object System.Text.UTF8Encoding($false)))
    $w.AutoFlush = $true
    $w.WriteLine('timestamp,title,pid,exe,is_source')

    $clock = [System.Diagnostics.Stopwatch]::StartNew()
    $rows = 0
    $lastKey = $null
    $lastSource = $false
    $lastTick = 0L
    $sourceMs = 0L
    try {
        while ($true) {
            $hwnd = 0L; $fgPid = [uint32]0; $title = ''; $exe = ''
            try { [UcrtFg]::Query([ref]$hwnd, [ref]$fgPid, [ref]$title, [ref]$exe) } catch { }
            if ($null -eq $title) { $title = '' }
            if ($null -eq $exe) { $exe = '' }
            $isSource = $false
            if ($Source.Length -gt 0) {
                $isSource = ($title.IndexOf($Source, [System.StringComparison]::OrdinalIgnoreCase) -ge 0)
            }
            $now = $clock.ElapsedMilliseconds
            if ($null -ne $lastKey -and $lastSource) { $sourceMs += ($now - $lastTick) }
            $lastTick = $now
            $lastSource = $isSource

            # Window identity only: pixel-demo and the ultimatecrt output rewrite their titles every second,
            # which is not a foreground change (2026-11-19 live run).
            $key = "${hwnd}|${fgPid}"
            if ($key -ne $lastKey) {
                $lastKey = $key
                $stamp = (Get-Date).ToString('yyyy-MM-ddTHH:mm:ss.fff')
                $flag = 0
                if ($isSource) { $flag = 1 }
                $w.WriteLine((@($stamp, (ConvertTo-CsvField $title), $fgPid, (ConvertTo-CsvField $exe), $flag) -join ','))
                $rows++
            }
            if ($Seconds -gt 0 -and $clock.ElapsedMilliseconds -ge ($Seconds * 1000)) { break }
            Start-Sleep -Milliseconds $Interval
        }
    }
    finally {
        $w.Dispose()
        $total = $clock.ElapsedMilliseconds
        $changes = [Math]::Max($rows - 1, 0)
        $pct = 0.0
        if ($total -gt 0) { $pct = 100.0 * $sourceMs / $total }
        Write-Host ('Duration: {0:N1} s' -f ($total / 1000.0))
        Write-Host ("Foreground changes: ${changes} (baseline row excluded)")
        if ($Source.Length -gt 0) {
            Write-Host ('Source in foreground: {0:N1} % of the time' -f $pct)
        } else {
            Write-Host 'Source in foreground: n/a (no -SourceTitle)'
        }
        Write-Host "CSV: ${full} (titles can be private: delete it after the run)"
    }
}

if ($SelfTest) {
    $tmp = Join-Path ([System.IO.Path]::GetTempPath()) ('poll-foreground-selftest-' + [guid]::NewGuid().ToString('N') + '.csv')
    Invoke-Poll $tmp '' 3 $IntervalMs
    $lines = @(Get-Content -LiteralPath $tmp)
    Remove-Item -LiteralPath $tmp -Force -ErrorAction SilentlyContinue
    $dataRows = $lines.Count - 1
    if ($dataRows -eq 1) {
        Write-Host 'SelfTest PASS: CSV holds exactly the baseline row.'
        exit 0
    }
    Write-Host "SelfTest FAIL: expected 1 data row, found ${dataRows} (did the foreground window change during the test?)."
    exit 1
}

Invoke-Poll $OutFile $SourceTitle $DurationSeconds $IntervalMs
