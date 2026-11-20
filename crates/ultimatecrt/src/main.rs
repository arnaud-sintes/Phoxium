//! ultimatecrt: captures a window, runs it through a virtual CRT tube and presents it in its own output window.
//! Started without options it opens that window and lists the windows to capture (A23); `ultimatecrt --help`
//! lists the developer and diagnostic options.
//!
//! Release builds are GUI programs (`windows_subsystem = "windows"`): no console window opens with the output
//! window. Started from a terminal with options (`--help`, `--list-windows`, `--stats`...), the process attaches
//! to that terminal's console so its text still prints there. A fatal error is shown in a message box when no
//! console shows it.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// The two console calls, declared here so the `windows` crate needs no extra feature.
#[link(name = "kernel32")]
extern "system" {
    fn AttachConsole(process_id: u32) -> i32;
    fn GetStdHandle(std_handle: u32) -> isize;
}

/// `ATTACH_PARENT_PROCESS`, `(DWORD)-1`.
const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
/// `STD_OUTPUT_HANDLE`, `(DWORD)-11`.
const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;

/// Whether standard output already goes somewhere (a console, a pipe, a file).
fn stdout_connected() -> bool {
    // SAFETY: plain query of the process's standard handle; null or INVALID_HANDLE_VALUE (-1) when none.
    let h = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
    h != 0 && h != -1
}

/// Attaches to the console of the parent process (the terminal that started us), when there is one and standard
/// output is not redirected already. Returns whether text now reaches a console or a redirection.
fn attach_parent_console() -> bool {
    if stdout_connected() {
        return true;
    }
    // SAFETY: plain call; it fails without a parent console, which leaves the process as it was.
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) != 0 }
}

/// R-48: DLLs loaded by name (`vulkan-1.dll` above all) come from System32 only, never from the folder the
/// executable was unzipped to, the current directory or `PATH` (DLL planting). DLLs loaded by full path (the
/// Vulkan drivers and layers) are not affected.
fn harden_dll_search() {
    use windows::Win32::System::LibraryLoader::{SetDefaultDllDirectories, LOAD_LIBRARY_SEARCH_SYSTEM32};
    // SAFETY: plain process-wide setting, made before any library is loaded by name.
    if let Err(e) = unsafe { SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32) } {
        eprintln!("warning: cannot restrict the DLL search path: {e}");
    }
}

fn main() {
    harden_dll_search();
    // Options are developer and diagnostic tools: their text goes to the terminal they came from.
    let has_args = std::env::args_os().len() > 1;
    let console = if cfg!(debug_assertions) {
        true
    } else if has_args {
        attach_parent_console()
    } else {
        stdout_connected()
    };
    if let Err(e) = ultimatecrt::app::run() {
        eprintln!("error: {e}");
        if !console {
            show_error(&e.to_string());
        }
        std::process::exit(1);
    }
}

/// A fatal error without a console (the program started from Explorer): a message box. The app has already
/// saved the settings and the burn-in state and closed its window (`app::run` shuts down on every exit path).
fn show_error(text: &str) {
    use windows::core::HSTRING;
    use windows::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, PeekMessageW, MB_ICONERROR, MB_OK, MSG, PM_REMOVE, WM_QUIT,
    };
    // SAFETY: plain calls with owned strings. A WM_QUIT left in the queue would close the box at once.
    unsafe {
        let mut msg = MSG::default();
        while PeekMessageW(&mut msg, None, WM_QUIT, WM_QUIT, PM_REMOVE).as_bool() {}
        MessageBoxW(
            None,
            &HSTRING::from(format!("ultimateCRT could not start or stopped:\n\n{text}")),
            &HSTRING::from("ultimateCRT"),
            MB_OK | MB_ICONERROR,
        );
    }
}
