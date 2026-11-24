//! One running Phoxium per user session. Two instances would share `settings.toml` (the last writer wins, the
//! other's changes are lost) and the burn-in wear maps; a second start is refused with a plain message instead.
//!
//! A named mutex of the session (`Local\...`) is held by the first instance for as long as it lives; Windows
//! releases it when the process ends, whatever way it ends. The developer option `--allow-multiple` skips the guard
//! (the live-check scripts and a build tested next to an installed one).

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
use windows::Win32::System::Threading::CreateMutexW;

/// The mutex of the application.
pub const MUTEX_NAME: &str = "Local\\Phoxium-single-instance";

/// What a second start is told.
pub const ALREADY_RUNNING: &str =
    "Phoxium is already running (one window at a time: two would share the settings and the burn-in state). \
     Close the other one first, or start this one with --allow-multiple.";

/// Another holder has the mutex.
#[derive(Debug, PartialEq, Eq)]
pub struct AlreadyRunning;

/// The held mutex: the instance is the only one while this lives.
#[derive(Debug)]
pub struct Guard(HANDLE);

impl Drop for Guard {
    fn drop(&mut self) {
        // SAFETY: the handle came from `CreateMutexW` and is closed once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Takes the mutex called `name`: `Ok` for the first holder, `Err` when another holder (a process, or an earlier
/// guard of this one) has it. Any other failure to create it lets the start go on (a guard that cannot be made must
/// not stop the program).
pub fn acquire(name: &str) -> Result<Option<Guard>, AlreadyRunning> {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `wide` is a zero-terminated UTF-16 string alive for the call; no security attributes, not owned initially
    // (existence is what is tested).
    unsafe {
        match CreateMutexW(None, false, PCWSTR(wide.as_ptr())) {
            Ok(h) => {
                if GetLastError() == ERROR_ALREADY_EXISTS {
                    let _ = CloseHandle(h);
                    Err(AlreadyRunning)
                } else {
                    Ok(Some(Guard(h)))
                }
            }
            Err(_) => Ok(None),
        }
    }
}

/// [`acquire`] for the application, as the error text of [`ALREADY_RUNNING`].
pub fn acquire_app() -> Result<Option<Guard>, String> {
    acquire(MUTEX_NAME).map_err(|AlreadyRunning| ALREADY_RUNNING.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first holder gets the mutex, a second one is refused while it is held, and it is free again once the
    /// first is gone (a mutex of its own name, so the real application's is not touched).
    #[test]
    fn a_second_holder_is_refused_until_the_first_is_gone() {
        let name = format!("Local\\Phoxium-test-instance-{}", std::process::id());
        let first = acquire(&name).expect("the first holder").expect("a mutex");
        assert!(acquire(&name).is_err(), "refused while held");
        assert!(acquire(&name).is_err(), "and again");
        drop(first);
        let again = acquire(&name).expect("free again");
        assert!(again.is_some());
    }

    #[test]
    fn the_message_names_the_way_out() {
        assert!(ALREADY_RUNNING.contains("--allow-multiple") && ALREADY_RUNNING.contains("already running"));
    }
}
