//! What the installed keyboard layouts type with AltGr (review D-01): the probe behind the Hotkeys rows' warning
//! for a `Ctrl+Alt+<character key>` chord (`hotkeys::altgr_conflict`). Windows sends AltGr as LeftCtrl+RightAlt,
//! so such a hotkey also fires on AltGr+key and swallows the character that key types on that layout.
//!
//! Read-only Win32 calls: `GetKeyboardLayoutList` (the layouts the user can switch to) and `ToUnicodeEx` with
//! Ctrl and Alt down, with the flag that leaves the keyboard state (dead keys) untouched (Windows 10 1607 and
//! later).

use windows::Win32::{
    Globalization::LCIDToLocaleName,
    UI::Input::KeyboardAndMouse::{GetKeyboardLayoutList, MapVirtualKeyExW, ToUnicodeEx, HKL, MAPVK_VK_TO_VSC},
};

/// `ToUnicodeEx` flag: do not change the keyboard state (Windows 10 1607 and later).
const NO_STATE_CHANGE: u32 = 0x4;

const VK_SHIFT: usize = 0x10;
const VK_CONTROL: usize = 0x11;
const VK_MENU: usize = 0x12;
const VK_LCONTROL: usize = 0xA2;
const VK_RMENU: usize = 0xA5;

/// The layouts installed for the user (at most 32).
fn layouts() -> Vec<HKL> {
    let mut list = [HKL::default(); 32];
    // SAFETY: the buffer is owned and its length is passed by the wrapper.
    let n = unsafe { GetKeyboardLayoutList(Some(&mut list)) };
    list[..usize::try_from(n).unwrap_or(0).min(list.len())].to_vec()
}

/// The locale name of a layout's language (`de-DE`), or its language id in hexadecimal.
fn layout_name(hkl: HKL) -> String {
    let lang = (hkl.0 as usize & 0xFFFF) as u32;
    let mut buf = [0u16; 85];
    // SAFETY: the buffer is owned and its length is passed by the wrapper.
    let n = unsafe { LCIDToLocaleName(lang, Some(&mut buf), 0) };
    if n > 1 {
        String::from_utf16_lossy(&buf[..(n as usize - 1)])
    } else {
        format!("{lang:04X}")
    }
}

/// What `vk` types with AltGr (and Shift when `shift`) on `hkl`: `Some` for a printable character or a dead key.
fn altgr_char(hkl: HKL, vk: u16, shift: bool) -> Option<String> {
    let mut state = [0u8; 256];
    for k in [VK_CONTROL, VK_LCONTROL, VK_MENU, VK_RMENU] {
        state[k] = 0x80;
    }
    if shift {
        state[VK_SHIFT] = 0x80;
    }
    let mut out = [0u16; 8];
    // SAFETY: plain value queries on owned buffers; NO_STATE_CHANGE leaves the thread's keyboard state as it is.
    let n = unsafe {
        let scan = MapVirtualKeyExW(u32::from(vk), MAPVK_VK_TO_VSC, Some(hkl));
        ToUnicodeEx(u32::from(vk), scan, &state, &mut out, NO_STATE_CHANGE, Some(hkl))
    };
    match n {
        n if n < 0 => Some("a dead key (an accent)".to_string()),
        0 => None,
        n => {
            let s: String = String::from_utf16_lossy(&out[..(n as usize).min(out.len())])
                .chars()
                .filter(|c| !c.is_control())
                .collect();
            (!s.is_empty()).then(|| format!("'{s}'"))
        }
    }
}

/// The `hotkeys::LayoutProbe` of the live app: what the first installed layout that types something with AltGr
/// on `vk` types, with the layout's name (`'@' (keyboard layout de-DE)`); `None` when no installed layout does.
pub fn altgr_text(vk: u16, shift: bool) -> Option<String> {
    layouts()
        .into_iter()
        .find_map(|hkl| altgr_char(hkl, vk, shift).map(|c| format!("{c} (keyboard layout {})", layout_name(hkl))))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Machine-dependent: only checks that the probe runs and never reports a function key or a navigation key,
    /// which type nothing on any layout.
    #[test]
    fn function_and_navigation_keys_type_nothing_under_altgr() {
        assert!(!layouts().is_empty(), "at least one layout is installed");
        for vk in [0x70u16, 0x76, 0x78, 0x23, 0x21, 0x22] {
            assert_eq!(altgr_text(vk, false), None, "VK {vk:#04X}");
            assert_eq!(altgr_text(vk, true), None, "VK {vk:#04X} with Shift");
        }
    }
}
