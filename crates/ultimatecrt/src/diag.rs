//! Diagnostics log (`--log <file.csv>`): per-second capture and render interval statistics, gaps and
//! foreground-window changes, plus a summary at exit. The statistics are pure and unit tested; only
//! `Foreground` and `Log` touch the operating system.

use std::{
    fs::File,
    io::{BufWriter, Write},
    time::Instant,
};

use windows::Win32::{
    Foundation::HWND,
    UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId},
};

use crate::gpu::FrameTimings;
use crate::Res;

/// An interval longer than this counts as a gap.
pub const GAP_MS: f64 = 50.0;

/// Summary of a set of intervals, in milliseconds. All zero when there are no intervals.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct IntervalStats {
    pub count: usize,
    pub min_ms: f64,
    pub max_ms: f64,
    pub p99_ms: f64,
    pub gaps: usize,
}

/// Collects intervals; `clear` keeps the allocation so the loop does not allocate per frame.
#[derive(Default)]
pub struct Intervals {
    values: Vec<f64>,
}

impl Intervals {
    pub fn push(&mut self, ms: f64) {
        self.values.push(ms);
    }

    pub fn clear(&mut self) {
        self.values.clear();
    }

    pub fn stats(&mut self) -> IntervalStats {
        stats_of(&mut self.values)
    }
}

/// Nearest-rank p99: the value at rank ceil(0.99 n) of the sorted data. Sorts `values` in place.
pub fn stats_of(values: &mut [f64]) -> IntervalStats {
    if values.is_empty() {
        return IntervalStats::default();
    }
    values.sort_by(f64::total_cmp);
    let n = values.len();
    let rank = ((n as f64 * 0.99).ceil() as usize).clamp(1, n);
    IntervalStats {
        count: n,
        min_ms: values[0],
        max_ms: values[n - 1],
        p99_ms: values[rank - 1],
        gaps: values.iter().filter(|&&v| v > GAP_MS).count(),
    }
}

/// The foreground window as last polled.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ForegroundInfo {
    pub hwnd: isize,
    pub pid: u32,
    pub title: String,
}

/// Polls `GetForegroundWindow` (no hooks) and reports changes.
pub struct Foreground {
    last: Option<isize>,
    source: isize,
}

impl Foreground {
    pub fn new(source: HWND) -> Self {
        Self {
            last: None,
            source: source.0 as isize,
        }
    }

    /// Returns the new foreground window when it differs from the previous poll (always on the first poll).
    pub fn poll(&mut self) -> Option<ForegroundInfo> {
        let hwnd = unsafe { GetForegroundWindow() };
        let key = hwnd.0 as isize;
        if self.last == Some(key) {
            return None;
        }
        self.last = Some(key);
        let mut pid = 0u32;
        let mut buf = [0u16; 256];
        let n = unsafe {
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            GetWindowTextW(hwnd, &mut buf)
        };
        Some(ForegroundInfo {
            hwnd: key,
            pid,
            title: String::from_utf16_lossy(&buf[..n.max(0) as usize]),
        })
    }

    pub fn is_source(&self) -> bool {
        self.last == Some(self.source)
    }
}

fn csv_field(text: &str) -> String {
    format!("\"{}\"", text.replace('"', "\"\"").replace(['\r', '\n'], " "))
}

#[derive(Default)]
struct Totals {
    seconds: usize,
    capture_gaps: usize,
    render_gaps: usize,
    capture_max_ms: f64,
    render_max_ms: f64,
    fps_sum: f64,
    fps_min: f64,
    fg_changes: usize,
    fg_not_source_seconds: usize,
}

/// The CSV log: one row per second, plus a summary printed at exit.
pub struct Log {
    out: BufWriter<File>,
    start: Instant,
    fg: Foreground,
    capture: Intervals,
    render: Intervals,
    last_capture_count: u64,
    last_capture_time: Instant,
    last_render_time: Instant,
    changes: usize,
    last_change: Option<ForegroundInfo>,
    totals: Totals,
    /// Names of the GPU timing segments, one `gpu_<name>_ms` column each (after `gpu_frame_ms`).
    gpu_names: Vec<&'static str>,
}

/// The columns before the GPU timings; their order never changes (readers index them by position).
const BASE_HEADER: &str = "t_s,capture_fps,cap_min_ms,cap_max_ms,cap_p99_ms,cap_gaps_50ms,render_fps,render_min_ms,\
                           render_max_ms,render_p99_ms,render_gaps_50ms,fg_changes,fg_pid,fg_title,fg_is_source";

/// The CSV header: the fixed columns, then `gpu_frame_ms` and one `gpu_<segment>_ms` per GPU timing
/// segment (T-21), appended so that the earlier columns keep their position.
pub fn csv_header(gpu_names: &[&str]) -> String {
    format!("{BASE_HEADER}{}", crate::gpu::csv_columns(gpu_names))
}

impl Log {
    /// `gpu_names` are the GPU timing segments ([`crate::gpu::Renderer::timing_names`]), the columns
    /// appended to the fixed ones.
    pub fn create(path: &str, source: HWND, gpu_names: Vec<&'static str>) -> Res<Self> {
        let mut out = BufWriter::new(File::create(path).map_err(|e| format!("cannot create log {path}: {e}"))?);
        writeln!(out, "{}", csv_header(&gpu_names))?;
        let now = Instant::now();
        Ok(Self {
            gpu_names,
            out,
            start: now,
            fg: Foreground::new(source),
            capture: Intervals::default(),
            render: Intervals::default(),
            last_capture_count: 0,
            last_capture_time: now,
            last_render_time: now,
            changes: 0,
            last_change: None,
            totals: Totals {
                fps_min: f64::MAX,
                ..Totals::default()
            },
        })
    }

    /// Call once per loop iteration, after the render, with the capture frame counter.
    /// Capture intervals are seen at loop granularity: a counter jump of k frames over d ms counts as d/k.
    pub fn tick(&mut self, captured: u64) {
        let now = Instant::now();
        self.render.push((now - self.last_render_time).as_secs_f64() * 1000.0);
        self.last_render_time = now;
        if captured > self.last_capture_count {
            let k = (captured - self.last_capture_count) as f64;
            let each = (now - self.last_capture_time).as_secs_f64() * 1000.0 / k;
            for _ in 0..(k as u64).min(1000) {
                self.capture.push(each);
            }
            self.last_capture_count = captured;
            self.last_capture_time = now;
        }
        if let Some(info) = self.fg.poll() {
            // The first poll is the baseline, not a change.
            if self.last_change.is_some() {
                self.changes += 1;
                self.totals.fg_changes += 1;
            }
            self.last_change = Some(info);
        }
    }

    /// Call once per second with the capture frames and render count of that second and its length, and
    /// the mean GPU timings of that second (empty columns when there are none).
    pub fn second(&mut self, captured_frames: u64, renders: u32, secs: f64, gpu: Option<&FrameTimings>) -> Res<()> {
        let c = self.capture.stats();
        let r = self.render.stats();
        let cap_fps = captured_frames as f64 / secs;
        let (pid, title) = if self.changes > 0 {
            self.last_change.as_ref().map_or((String::new(), String::new()), |i| {
                (i.pid.to_string(), csv_field(&i.title))
            })
        } else {
            (String::new(), String::new())
        };
        let is_source = self.fg.is_source();
        writeln!(
            self.out,
            "{:.1},{:.1},{:.2},{:.2},{:.2},{},{:.1},{:.2},{:.2},{:.2},{},{},{},{},{}{}",
            self.start.elapsed().as_secs_f64(),
            cap_fps,
            c.min_ms,
            c.max_ms,
            c.p99_ms,
            c.gaps,
            renders as f64 / secs,
            r.min_ms,
            r.max_ms,
            r.p99_ms,
            r.gaps,
            self.changes,
            pid,
            title,
            u8::from(is_source),
            FrameTimings::csv_values(gpu, &self.gpu_names)
        )?;
        self.out.flush()?;
        let t = &mut self.totals;
        t.seconds += 1;
        t.capture_gaps += c.gaps;
        t.render_gaps += r.gaps;
        t.capture_max_ms = t.capture_max_ms.max(c.max_ms);
        t.render_max_ms = t.render_max_ms.max(r.max_ms);
        t.fps_sum += cap_fps;
        t.fps_min = t.fps_min.min(cap_fps);
        if !is_source {
            t.fg_not_source_seconds += 1;
        }
        self.changes = 0;
        self.capture.clear();
        self.render.clear();
        Ok(())
    }

    pub fn summary(&mut self) {
        let t = &self.totals;
        if t.seconds == 0 {
            println!("log summary: no complete second recorded");
            return;
        }
        println!(
            "log summary: {} s | capture fps avg {:.1} min {:.1} | capture gaps >{GAP_MS:.0} ms: {} (max interval {:.1} ms) | \
             render gaps >{GAP_MS:.0} ms: {} (max interval {:.1} ms) | foreground changes: {} | seconds with foreground not the source: {}",
            t.seconds,
            t.fps_sum / t.seconds as f64,
            t.fps_min,
            t.capture_gaps,
            t.capture_max_ms,
            t.render_gaps,
            t.render_max_ms,
            t.fg_changes,
            t.fg_not_source_seconds
        );
        let _ = self.out.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_zero() {
        assert_eq!(stats_of(&mut []), IntervalStats::default());
    }

    #[test]
    fn steady_60_fps_has_no_gap() {
        let mut v = vec![16.7; 60];
        let s = stats_of(&mut v);
        assert_eq!((s.count, s.gaps), (60, 0));
        assert_eq!((s.min_ms, s.max_ms, s.p99_ms), (16.7, 16.7, 16.7));
    }

    #[test]
    fn p99_is_nearest_rank() {
        // 1..=100 ms: ceil(0.99 * 100) = rank 99 -> 99 ms; max is 100.
        let mut v: Vec<f64> = (1..=100).rev().map(f64::from).collect();
        let s = stats_of(&mut v);
        assert_eq!((s.min_ms, s.max_ms, s.p99_ms), (1.0, 100.0, 99.0));
    }

    #[test]
    fn p99_of_few_values_is_the_max() {
        let mut v = vec![10.0, 12.0, 90.0];
        assert_eq!(stats_of(&mut v).p99_ms, 90.0);
    }

    #[test]
    fn counts_only_intervals_strictly_over_50_ms() {
        let mut v = vec![16.0, 50.0, 50.1, 120.0, 16.0, 300.0];
        let s = stats_of(&mut v);
        assert_eq!(s.gaps, 3);
        assert_eq!(s.max_ms, 300.0);
    }

    #[test]
    fn intervals_clear_resets() {
        let mut i = Intervals::default();
        i.push(70.0);
        assert_eq!(i.stats().gaps, 1);
        i.clear();
        assert_eq!(i.stats(), IntervalStats::default());
    }

    #[test]
    fn header_keeps_the_fixed_columns_first_and_appends_the_gpu_ones() {
        let fixed: Vec<&str> = BASE_HEADER.split(',').collect();
        assert_eq!(fixed.len(), 15);
        assert_eq!(&fixed[..3], ["t_s", "capture_fps", "cap_min_ms"]);
        assert_eq!(&fixed[11..], ["fg_changes", "fg_pid", "fg_title", "fg_is_source"]);
        let h = csv_header(&["upload", "decode", "encode"]);
        let cols: Vec<&str> = h.split(',').collect();
        assert_eq!(&cols[..15], &fixed[..]);
        assert_eq!(
            &cols[15..],
            ["gpu_frame_ms", "gpu_upload_ms", "gpu_decode_ms", "gpu_encode_ms"]
        );
        // With no timing segments only the frame column is added.
        assert_eq!(csv_header(&[]), format!("{BASE_HEADER},gpu_frame_ms"));
    }

    #[test]
    fn a_row_has_one_value_per_header_column() {
        // The fixed part of a row holds 15 fields (the title is quoted, here empty).
        let names = ["upload", "decode", "encode"];
        let t = FrameTimings {
            frames: 60,
            frame_ms: 0.5,
            stages: names
                .iter()
                .map(|&name| crate::gpu::StageTiming {
                    name,
                    ms: (name != "upload").then_some(0.25),
                    bypassed: false,
                })
                .collect(),
        };
        let fixed_row = "1.0,60.0,16.60,16.70,16.70,0,60.0,16.60,16.70,16.70,0,0,,,1";
        for gpu in [Some(&t), None] {
            let row = format!("{fixed_row}{}", FrameTimings::csv_values(gpu, &names));
            assert_eq!(row.split(',').count(), csv_header(&names).split(',').count());
        }
        let row = format!("{fixed_row}{}", FrameTimings::csv_values(Some(&t), &names));
        assert!(row.ends_with(",1,0.500,,0.250,0.250"), "{row}");
    }

    #[test]
    fn csv_field_quotes() {
        assert_eq!(csv_field("a,\"b\"\n"), "\"a,\"\"b\"\" \"");
    }
}
