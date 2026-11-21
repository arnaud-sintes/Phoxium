<#
.SYNOPSIS
    Independent focus monitor (T-07): logs every foreground, focus and minimise event with a WinEvent hook.
.DESCRIPTION
    Hooks EVENT_SYSTEM_FOREGROUND, EVENT_OBJECT_FOCUS (window-level changes only) and
    EVENT_SYSTEM_MINIMIZESTART/END out of context, pumps messages on its own thread, and writes one CSV row
    per event: wall time (ISO, ms), t_ms (monotonic ms since the monitor start), event, hwnd, class, title,
    pid, exe, and the NOACTIVATE / TOPMOST / TOOLWINDOW extended styles of the window. A baseline row
    (event "baseline") gives the foreground window at start. The CSV is flushed after each row.
    It does not poll and does not depend on the product. Window titles can hold private content: delete the
    CSV after the run.
.PARAMETER OutFile
    CSV path (overwritten).
.PARAMETER DurationSeconds
    Run time in seconds.
#>
param(
    [Parameter(Mandatory = $true)][string]$OutFile,
    [int]$DurationSeconds = 60
)

$ErrorActionPreference = 'Stop'

if (-not ('PhoxiumFocusMon' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
public static class PhoxiumFocusMon {
    delegate void WinEventProc(IntPtr hook, uint ev, IntPtr hwnd, int idObject, int idChild, uint thread, uint time);
    [DllImport("user32.dll")] static extern IntPtr SetWinEventHook(uint min, uint max, IntPtr mod, WinEventProc proc, uint pid, uint tid, uint flags);
    [DllImport("user32.dll")] static extern bool UnhookWinEvent(IntPtr h);
    [DllImport("user32.dll")] static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] static extern IntPtr GetAncestor(IntPtr h, uint flags);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassName(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll", EntryPoint = "GetWindowLongPtrW")] static extern IntPtr GetWindowLongPtr(IntPtr h, int idx);
    [DllImport("user32.dll")] static extern uint MsgWaitForMultipleObjects(uint n, IntPtr[] handles, bool all, uint ms, uint mask);
    [DllImport("user32.dll")] static extern bool PeekMessage(out MSG m, IntPtr h, uint min, uint max, uint remove);
    [DllImport("user32.dll")] static extern bool TranslateMessage(ref MSG m);
    [DllImport("user32.dll")] static extern IntPtr DispatchMessage(ref MSG m);
    [DllImport("kernel32.dll")] static extern IntPtr OpenProcess(uint access, bool inherit, uint pid);
    [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr h);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode)] static extern bool QueryFullProcessImageName(IntPtr h, uint flags, StringBuilder s, ref int n);
    [StructLayout(LayoutKind.Sequential)] struct MSG { public IntPtr hwnd; public uint message; public IntPtr wParam; public IntPtr lParam; public uint time; public int x; public int y; }

    static WinEventProc proc = OnEvent; // kept alive for the hook
    static StreamWriter w;
    static Stopwatch sw;
    static IntPtr lastFocusTop = IntPtr.Zero;
    public static int ForegroundEvents;

    static string Csv(string s) { return "\"" + (s ?? "").Replace("\"", "\"\"") + "\""; }

    static void Row(string ev, IntPtr h) {
        string title = "", cls = "", exe = ""; uint pid = 0; long ex = 0;
        if (h != IntPtr.Zero) {
            try { var sb = new StringBuilder(512); GetWindowText(h, sb, 512); title = sb.ToString(); } catch { }
            try { var sb = new StringBuilder(256); GetClassName(h, sb, 256); cls = sb.ToString(); } catch { }
            try { ex = GetWindowLongPtr(h, -20).ToInt64(); } catch { }
            try {
                GetWindowThreadProcessId(h, out pid);
                IntPtr p = OpenProcess(0x1000, false, pid);
                if (p != IntPtr.Zero) {
                    var sb = new StringBuilder(1024); int n = sb.Capacity;
                    if (QueryFullProcessImageName(p, 0, sb, ref n)) exe = Path.GetFileName(sb.ToString());
                    CloseHandle(p);
                }
            } catch { }
        }
        bool noact = (ex & 0x08000000) != 0, top = (ex & 0x8) != 0, tool = (ex & 0x80) != 0;
        w.WriteLine(string.Join(",", new string[] {
            DateTime.Now.ToString("HH:mm:ss.fff"), sw.ElapsedMilliseconds.ToString(), ev, "0x" + h.ToInt64().ToString("X"),
            Csv(cls), Csv(title), pid.ToString(), Csv(exe), noact ? "1" : "0", top ? "1" : "0", tool ? "1" : "0" }));
        w.Flush();
    }

    static void OnEvent(IntPtr hook, uint ev, IntPtr hwnd, int idObject, int idChild, uint thread, uint time) {
        try {
            if (ev == 0x0003) { ForegroundEvents++; Row("foreground", hwnd); }
            else if (ev == 0x8005) {
                if (hwnd == IntPtr.Zero) return;
                IntPtr top = GetAncestor(hwnd, 2); // GA_ROOT
                if (top == lastFocusTop) return; // only window-level focus changes
                lastFocusTop = top; Row("focus", top);
            }
            else if (ev == 0x0016) { if (idObject == 0) Row("minimizestart", hwnd); }
            else if (ev == 0x0017) { if (idObject == 0) Row("minimizeend", hwnd); }
        } catch { }
    }

    public static void Run(string outFile, int seconds) {
        sw = Stopwatch.StartNew();
        w = new StreamWriter(outFile, false, new UTF8Encoding(false));
        w.WriteLine("time,t_ms,event,hwnd,class,title,pid,exe,noactivate,topmost,toolwindow");
        IntPtr fg = GetForegroundWindow();
        Row("baseline", fg);
        lastFocusTop = fg;
        const uint OUT = 0x0000 | 0x0002; // WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS
        IntPtr h1 = SetWinEventHook(0x0003, 0x0003, IntPtr.Zero, proc, 0, 0, OUT);
        IntPtr h2 = SetWinEventHook(0x0016, 0x0017, IntPtr.Zero, proc, 0, 0, OUT);
        IntPtr h3 = SetWinEventHook(0x8005, 0x8005, IntPtr.Zero, proc, 0, 0, OUT);
        long end = (long)seconds * 1000;
        MSG m;
        while (sw.ElapsedMilliseconds < end) {
            long left = end - sw.ElapsedMilliseconds;
            MsgWaitForMultipleObjects(0, null, false, (uint)Math.Max(1, Math.Min(left, 200)), 0x04FF);
            while (PeekMessage(out m, IntPtr.Zero, 0, 0, 1)) { TranslateMessage(ref m); DispatchMessage(ref m); }
        }
        UnhookWinEvent(h1); UnhookWinEvent(h2); UnhookWinEvent(h3);
        Row("end", GetForegroundWindow());
        w.Close();
    }
}
'@
}

[PhoxiumFocusMon]::Run([IO.Path]::GetFullPath($OutFile), $DurationSeconds)
Write-Output ("focus-monitor: {0} foreground events in {1} s -> {2}" -f [PhoxiumFocusMon]::ForegroundEvents, $DurationSeconds, $OutFile)
