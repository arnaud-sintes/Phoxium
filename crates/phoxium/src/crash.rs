//! Panic handling (review D-05). A release build is a GUI program without a console: a panic would make the
//! window vanish with no message, and skip the shutdown saves (settings, burn-in map).
//!
//! - [`install_hook`] (first thing in `main`): every panic, on any thread, writes a crash report
//!   `crash-<UTC timestamp>.txt` (version, thread, message, location, backtrace when `RUST_BACKTRACE` enables
//!   it) into the settings folder `%APPDATA%\Phoxium` (the temporary folder without `APPDATA`), keeping at
//!   most [`MAX_CRASH_FILES`] reports (the oldest are deleted), then runs the default hook (stderr).
//! - [`guard`] (around the session loop, `app.rs`): a panic in the loop becomes an error, so the shutdown
//!   (`app::shut_down`) still runs; the error names the report, and `main` shows it in a message box.
//! - A window procedure cannot unwind into Windows (an `extern "system"` function aborts the process on a
//!   panic): `output.rs` catches it there, answers with `DefWindowProcW` and records it
//!   ([`record_callback_panic`]); the loop ends on it at its next iteration ([`take_callback_panic`]).
//! - A panic outside the loop (start-up) reaches `main`, which shows the same message box.
//!
//! A panic on another thread (capture callbacks, the settings writer) is written to a report; that thread
//! ends, the app goes on.

use std::any::Any;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::Res;

/// Most crash reports kept in the folder: writing a new one deletes the oldest beyond this.
pub const MAX_CRASH_FILES: usize = 5;

/// The last report written by the hook (its path), for the error and the message box.
static LAST_REPORT: Mutex<Option<PathBuf>> = Mutex::new(None);
/// A panic caught in a window procedure, not yet reported by the loop.
static CALLBACK_PANIC: Mutex<Option<String>> = Mutex::new(None);

/// The text of a panic payload (`panic!` with a literal or a formatted message), or a placeholder.
pub fn panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "(no message)".to_string()
    }
}

/// UTC civil date and time of `unix_secs`: (year, month, day, hour, minute, second).
pub fn utc(unix_secs: u64) -> (i64, u32, u32, u32, u32, u32) {
    let days = (unix_secs / 86_400) as i64;
    let rem = unix_secs % 86_400;
    // Days to civil date (H. Hinnant's algorithm, proleptic Gregorian).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (
        year,
        month,
        day,
        (rem / 3600) as u32,
        (rem / 60 % 60) as u32,
        (rem % 60) as u32,
    )
}

/// The report's file name for `unix_secs`: `crash-YYYYMMDD-HHMMSS.txt` (UTC; the names sort by time), with
/// `-n` before the extension for the n-th report of the same second (n >= 2).
pub fn file_name(unix_secs: u64, n: u32) -> String {
    let (y, mo, d, h, mi, s) = utc(unix_secs);
    let suffix = if n >= 2 { format!("-{n}") } else { String::new() };
    format!("crash-{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}{suffix}.txt")
}

/// The report's text.
pub fn report_text(
    version: &str,
    unix_secs: u64,
    thread: &str,
    message: &str,
    location: &str,
    backtrace: &str,
) -> String {
    let (y, mo, d, h, mi, s) = utc(unix_secs);
    format!(
        "Phoxium {version} crashed (panic)\n\
         time: {y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02} UTC\n\
         thread: {thread}\n\
         location: {location}\n\
         message: {message}\n\
         \n\
         backtrace:\n{backtrace}\n"
    )
}

/// Of the report file names `names` (any order), those to delete so that `keep` remain: the oldest (the names
/// sort by time). Other file names are ignored.
pub fn to_delete(names: &[String], keep: usize) -> Vec<String> {
    let mut reports: Vec<&String> = names
        .iter()
        .filter(|n| n.starts_with("crash-") && n.ends_with(".txt"))
        .collect();
    // By time, then by the same-second number (a plain name is number 1; "-2" would sort before ".txt").
    let key = |n: &String| {
        let stem = &n["crash-".len()..n.len() - ".txt".len()];
        let (time, seq) = match stem.get(15..) {
            Some(rest) if !rest.is_empty() => (&stem[..15], rest.trim_start_matches('-').parse().unwrap_or(0)),
            _ => (stem, 1),
        };
        (time.to_string(), seq)
    };
    reports.sort_by_key(|n| key(n));
    let extra = reports.len().saturating_sub(keep);
    reports[..extra].iter().map(|n| (*n).clone()).collect()
}

/// Writes `text` as a new report in `dir` (created when missing), after deleting the oldest reports so that
/// at most [`MAX_CRASH_FILES`] remain with the new one. Returns its path.
pub fn write_report(dir: &Path, unix_secs: u64, text: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let names: Vec<String> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    for old in to_delete(&names, MAX_CRASH_FILES - 1) {
        let _ = std::fs::remove_file(dir.join(old));
    }
    let mut n = 1;
    loop {
        let path = dir.join(file_name(unix_secs, n));
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut f) => {
                use std::io::Write;
                f.write_all(text.as_bytes())?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && n < 1000 => n += 1,
            Err(e) => return Err(e),
        }
    }
}

/// The folder of the reports: the settings folder, else the temporary folder.
pub fn report_dir() -> PathBuf {
    crate::settings::default_dir().unwrap_or_else(std::env::temp_dir)
}

/// Installs the panic hook (see the module documentation). Call it first in `main`.
pub fn install_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let message = panic_message(info.payload());
        let location = info.location().map_or_else(
            || "unknown".to_string(),
            |l| format!("{}:{}:{}", l.file(), l.line(), l.column()),
        );
        let thread = std::thread::current().name().unwrap_or("unnamed").to_string();
        let backtrace = std::backtrace::Backtrace::capture();
        let backtrace = match backtrace.status() {
            std::backtrace::BacktraceStatus::Captured => backtrace.to_string(),
            _ => "(not captured: set RUST_BACKTRACE=1 to include it)".to_string(),
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let text = report_text(env!("CARGO_PKG_VERSION"), now, &thread, &message, &location, &backtrace);
        match write_report(&report_dir(), now, &text) {
            Ok(path) => {
                if let Ok(mut last) = LAST_REPORT.lock() {
                    *last = Some(path);
                }
            }
            Err(e) => eprintln!("error: cannot write the crash report: {e}"),
        }
        default(info);
    }));
}

/// The path of the last report the hook wrote, if any.
pub fn last_report() -> Option<PathBuf> {
    LAST_REPORT.lock().ok().and_then(|l| l.clone())
}

/// The message of a panic `message` for the user: what happened and where the report is (`report`).
pub fn panic_text(message: &str, report: Option<&Path>) -> String {
    let where_ = report.map_or_else(
        || "No crash report could be written.".to_string(),
        |p| format!("A crash report was written to:\n{}", p.display()),
    );
    format!("internal error (panic): {message}\n\n{where_}")
}

/// Runs `body`; a panic in it becomes an error ([`panic_text`]) instead of unwinding further, so the caller's
/// shutdown runs. The panic hook has written the report by then.
pub fn guard<T>(body: impl FnOnce() -> Res<T>) -> Res<T> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)) {
        Ok(r) => r,
        Err(payload) => Err(panic_text(&panic_message(&*payload), last_report().as_deref()).into()),
    }
}

/// Records a panic caught in a window procedure or in a capture callback (`payload`), for the loop to report.
pub fn record_callback_panic(payload: &(dyn Any + Send)) {
    if let Ok(mut p) = CALLBACK_PANIC.lock() {
        if p.is_none() {
            *p = Some(panic_message(payload));
        }
    }
}

/// The panic caught in a window procedure since the last call, as the error the loop ends on.
pub fn take_callback_panic() -> Option<String> {
    let message = CALLBACK_PANIC.lock().ok()?.take()?;
    Some(panic_text(
        &format!("{message} (in a window procedure or a capture callback)"),
        last_report().as_deref(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_dates_and_names() {
        assert_eq!(utc(0), (1970, 1, 1, 0, 0, 0));
        // 2000-02-29 12:34:56 UTC (a leap day).
        assert_eq!(utc(951_827_696), (2000, 2, 29, 12, 34, 56));
        // 2026-11-20 23:59:59 UTC.
        assert_eq!(utc(1_795_219_199), (2026, 11, 20, 23, 59, 59));
        assert_eq!(file_name(1_795_219_199, 1), "crash-20261120-235959.txt");
        assert_eq!(file_name(1_795_219_199, 2), "crash-20261120-235959-2.txt");
    }

    #[test]
    fn report_text_and_messages() {
        let t = report_text("0.3.0", 0, "main", "boom", "src/app.rs:1:2", "(none)");
        for part in [
            "0.3.0",
            "1970-01-01 00:00:00 UTC",
            "thread: main",
            "src/app.rs:1:2",
            "message: boom",
        ] {
            assert!(t.contains(part), "{part}: {t}");
        }
        assert_eq!(panic_message(&"literal"), "literal");
        assert_eq!(panic_message(&String::from("formatted 3")), "formatted 3");
        assert_eq!(panic_message(&42_u32), "(no message)");
        let m = panic_text("boom", Some(Path::new(r"C:\x\crash-1.txt")));
        assert!(m.contains("boom") && m.contains(r"C:\x\crash-1.txt"), "{m}");
        assert!(panic_text("boom", None).contains("No crash report"));
    }

    /// At most MAX_CRASH_FILES reports: the oldest go; other files stay.
    #[test]
    fn reports_rotate() {
        let names: Vec<String> = [
            "crash-20260101-000003.txt",
            "settings.toml",
            "crash-20260101-000001.txt",
            "crash-20260101-000002.txt",
            "crash-20260101-000001-2.txt",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            to_delete(&names, 2),
            ["crash-20260101-000001.txt", "crash-20260101-000001-2.txt"]
        );
        assert_eq!(to_delete(&names, 3), ["crash-20260101-000001.txt"]);
        assert!(to_delete(&names, 5).is_empty());

        let dir = std::env::temp_dir().join(format!("phoxium-crash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("settings.toml"), "x").unwrap();
        let mut paths = Vec::new();
        for i in 0..8u64 {
            paths.push(write_report(&dir, 1_700_000_000 + i, &format!("report {i}")).unwrap());
        }
        // Two in the same second get distinct names.
        let same = write_report(&dir, 1_700_000_007, "again").unwrap();
        assert!(same.to_string_lossy().ends_with("-2.txt"), "{same:?}");
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(left.len(), MAX_CRASH_FILES + 1, "{left:?}"); // + settings.toml
        assert!(left.contains(&"settings.toml".to_string()));
        assert_eq!(std::fs::read_to_string(&same).unwrap(), "again");
        assert!(!paths[0].exists() && paths[7].exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A panic in the guarded body comes back as an error naming the message; Ok and Err pass through.
    #[test]
    fn guard_turns_a_panic_into_an_error() {
        let e = guard(|| -> Res<()> { panic!("simulated {}", 7) })
            .unwrap_err()
            .to_string();
        assert!(e.contains("internal error (panic): simulated 7"), "{e}");
        assert_eq!(guard(|| Ok(3)).unwrap(), 3);
        assert_eq!(
            guard(|| -> Res<()> { Err("plain".into()) }).unwrap_err().to_string(),
            "plain"
        );
    }

    /// A window procedure's panic is recorded once and taken once.
    #[test]
    fn callback_panics_are_recorded_and_taken() {
        let _ = take_callback_panic();
        record_callback_panic(&"in wndproc");
        record_callback_panic(&"second");
        let m = take_callback_panic().unwrap();
        assert!(m.contains("in wndproc") && m.contains("window procedure"), "{m}");
        assert!(take_callback_panic().is_none());
    }
}
