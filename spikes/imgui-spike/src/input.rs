//! Raw Win32 window messages -> Dear ImGui IO events, as pure functions (no window, no OS call).
//!
//! dear-imgui-rs has no Win32 platform backend (winit and SDL3 only), so the output window feeds
//! ImGui by hand. [`Win32Input::translate`] turns one message into zero or more [`UiEvent`]s;
//! [`apply`] pushes them into an `Io`. The caller passes the message number and its `WPARAM` and
//! `LPARAM` as plain integers, so the mapping is unit tested without any window. Behaviour follows
//! Dear ImGui's own `imgui_impl_win32.cpp` (MIT) where it matters: wheel sign and scale, left/right
//! modifiers from the scan code and extended bit, keypad Enter, mouse leave.

use dear_imgui_rs::{Io, Key, MouseButton};

// Window messages (winuser.h).
pub const WM_SETFOCUS: u32 = 0x0007;
pub const WM_KILLFOCUS: u32 = 0x0008;
pub const WM_KEYDOWN: u32 = 0x0100;
pub const WM_KEYUP: u32 = 0x0101;
pub const WM_CHAR: u32 = 0x0102;
pub const WM_SYSKEYDOWN: u32 = 0x0104;
pub const WM_SYSKEYUP: u32 = 0x0105;
pub const WM_MOUSEMOVE: u32 = 0x0200;
pub const WM_LBUTTONDOWN: u32 = 0x0201;
pub const WM_LBUTTONUP: u32 = 0x0202;
pub const WM_LBUTTONDBLCLK: u32 = 0x0203;
pub const WM_RBUTTONDOWN: u32 = 0x0204;
pub const WM_RBUTTONUP: u32 = 0x0205;
pub const WM_RBUTTONDBLCLK: u32 = 0x0206;
pub const WM_MBUTTONDOWN: u32 = 0x0207;
pub const WM_MBUTTONUP: u32 = 0x0208;
pub const WM_MBUTTONDBLCLK: u32 = 0x0209;
pub const WM_MOUSEWHEEL: u32 = 0x020A;
pub const WM_XBUTTONDOWN: u32 = 0x020B;
pub const WM_XBUTTONUP: u32 = 0x020C;
pub const WM_XBUTTONDBLCLK: u32 = 0x020D;
pub const WM_MOUSEHWHEEL: u32 = 0x020E;
pub const WM_MOUSELEAVE: u32 = 0x02A3;
pub const WM_DPICHANGED: u32 = 0x02E0;

const WHEEL_DELTA: f32 = 120.0;
const VK_SHIFT: usize = 0x10;
const VK_CONTROL: usize = 0x11;
const VK_MENU: usize = 0x12;
const VK_RETURN: usize = 0x0D;
const VK_LWIN: usize = 0x5B;
const VK_RWIN: usize = 0x5C;
/// Scan code of the right Shift key (left Shift is 0x2A).
const SCAN_RSHIFT: u32 = 0x36;

/// One input event for Dear ImGui (or for the caller, for `DpiScale`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum UiEvent {
    MousePos([f32; 2]),
    MouseButton(MouseButton, bool),
    /// Horizontal, vertical wheel steps (ImGui convention: +y scrolls up, +x scrolls left).
    Wheel([f32; 2]),
    Key(Key, bool),
    Char(char),
    Focus(bool),
    /// New DPI scale (dpi / 96), from `WM_DPICHANGED`; the caller rescales the style and fonts.
    DpiScale(f32),
}

/// Modifier state as ImGui sees it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub win: bool,
}

/// Translation state kept between messages: the modifiers and a pending UTF-16 high surrogate.
#[derive(Debug, Default)]
pub struct Win32Input {
    mods: Modifiers,
    high_surrogate: Option<u16>,
}

/// DPI scale factor of a monitor DPI (96 = 100 %).
pub fn dpi_scale(dpi: u32) -> f32 {
    dpi as f32 / 96.0
}

fn lo_i16(v: isize) -> i16 {
    (v as usize & 0xFFFF) as u16 as i16
}

fn hi_i16(v: isize) -> i16 {
    ((v as usize >> 16) & 0xFFFF) as u16 as i16
}

/// Win32 virtual key -> ImGui key. `extended` is bit 24 of `LPARAM`, `scan` its bits 16-23.
pub fn map_vk(vk: usize, extended: bool, scan: u32) -> Option<Key> {
    let k = match vk {
        0x09 => Key::Tab,
        0x25 => Key::LeftArrow,
        0x27 => Key::RightArrow,
        0x26 => Key::UpArrow,
        0x28 => Key::DownArrow,
        0x21 => Key::PageUp,
        0x22 => Key::PageDown,
        0x24 => Key::Home,
        0x23 => Key::End,
        0x2D => Key::Insert,
        0x2E => Key::Delete,
        0x08 => Key::Backspace,
        0x20 => Key::Space,
        VK_RETURN if extended => Key::KeypadEnter,
        VK_RETURN => Key::Enter,
        0x1B => Key::Escape,
        VK_SHIFT if scan == SCAN_RSHIFT => Key::RightShift,
        VK_SHIFT => Key::LeftShift,
        VK_CONTROL if extended => Key::RightCtrl,
        VK_CONTROL => Key::LeftCtrl,
        VK_MENU if extended => Key::RightAlt,
        VK_MENU => Key::LeftAlt,
        0xA0 => Key::LeftShift,
        0xA1 => Key::RightShift,
        0xA2 => Key::LeftCtrl,
        0xA3 => Key::RightCtrl,
        0xA4 => Key::LeftAlt,
        0xA5 => Key::RightAlt,
        VK_LWIN => Key::LeftSuper,
        VK_RWIN => Key::RightSuper,
        0x5D => Key::Menu,
        0x30..=0x39 => DIGITS[vk - 0x30],
        0x41..=0x5A => LETTERS[vk - 0x41],
        0x60..=0x69 => KEYPAD[vk - 0x60],
        0x6A => Key::KeypadMultiply,
        0x6B => Key::KeypadAdd,
        0x6D => Key::KeypadSubtract,
        0x6E => Key::KeypadDecimal,
        0x6F => Key::KeypadDivide,
        0x70..=0x87 => FUNCTION[vk - 0x70],
        0xBA => Key::Semicolon,
        0xBB => Key::Equal,
        0xBC => Key::Comma,
        0xBD => Key::Minus,
        0xBE => Key::Period,
        0xBF => Key::Slash,
        0xC0 => Key::GraveAccent,
        0xDB => Key::LeftBracket,
        0xDC => Key::Backslash,
        0xDD => Key::RightBracket,
        0xDE => Key::Apostrophe,
        0xE2 => Key::Oem102,
        0x14 => Key::CapsLock,
        0x91 => Key::ScrollLock,
        0x90 => Key::NumLock,
        0x2C => Key::PrintScreen,
        0x13 => Key::Pause,
        _ => return None,
    };
    Some(k)
}

const DIGITS: [Key; 10] = [
    Key::Key0,
    Key::Key1,
    Key::Key2,
    Key::Key3,
    Key::Key4,
    Key::Key5,
    Key::Key6,
    Key::Key7,
    Key::Key8,
    Key::Key9,
];
const KEYPAD: [Key; 10] = [
    Key::Keypad0,
    Key::Keypad1,
    Key::Keypad2,
    Key::Keypad3,
    Key::Keypad4,
    Key::Keypad5,
    Key::Keypad6,
    Key::Keypad7,
    Key::Keypad8,
    Key::Keypad9,
];
const LETTERS: [Key; 26] = [
    Key::A,
    Key::B,
    Key::C,
    Key::D,
    Key::E,
    Key::F,
    Key::G,
    Key::H,
    Key::I,
    Key::J,
    Key::K,
    Key::L,
    Key::M,
    Key::N,
    Key::O,
    Key::P,
    Key::Q,
    Key::R,
    Key::S,
    Key::T,
    Key::U,
    Key::V,
    Key::W,
    Key::X,
    Key::Y,
    Key::Z,
];
const FUNCTION: [Key; 24] = [
    Key::F1,
    Key::F2,
    Key::F3,
    Key::F4,
    Key::F5,
    Key::F6,
    Key::F7,
    Key::F8,
    Key::F9,
    Key::F10,
    Key::F11,
    Key::F12,
    Key::F13,
    Key::F14,
    Key::F15,
    Key::F16,
    Key::F17,
    Key::F18,
    Key::F19,
    Key::F20,
    Key::F21,
    Key::F22,
    Key::F23,
    Key::F24,
];

impl Win32Input {
    pub fn new() -> Self {
        Self::default()
    }

    /// Current modifier state.
    pub fn modifiers(&self) -> Modifiers {
        self.mods
    }

    /// Sets the modifier state (for example from `GetKeyState` when the window gains the focus,
    /// for keys already held) and emits the `Mod*` events that changed.
    pub fn set_modifiers(&mut self, m: Modifiers, out: &mut Vec<UiEvent>) {
        let old = self.mods;
        for (was, now, key) in [
            (old.ctrl, m.ctrl, Key::ModCtrl),
            (old.shift, m.shift, Key::ModShift),
            (old.alt, m.alt, Key::ModAlt),
            (old.win, m.win, Key::ModSuper),
        ] {
            if was != now {
                out.push(UiEvent::Key(key, now));
            }
        }
        self.mods = m;
    }

    /// Translates one window message. Returns true when the message is an input message that
    /// ImGui consumed (the window procedure then returns 0 instead of `DefWindowProc`, except for
    /// `WM_SYSKEY*`, which the caller should still pass on so that Alt+F4 keeps working).
    pub fn translate(&mut self, msg: u32, wparam: usize, lparam: isize, out: &mut Vec<UiEvent>) -> bool {
        match msg {
            WM_MOUSEMOVE => {
                out.push(UiEvent::MousePos([
                    f32::from(lo_i16(lparam)),
                    f32::from(hi_i16(lparam)),
                ]));
            }
            WM_MOUSELEAVE => out.push(UiEvent::MousePos([-f32::MAX, -f32::MAX])),
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => out.push(UiEvent::MouseButton(MouseButton::Left, true)),
            WM_LBUTTONUP => out.push(UiEvent::MouseButton(MouseButton::Left, false)),
            WM_RBUTTONDOWN | WM_RBUTTONDBLCLK => out.push(UiEvent::MouseButton(MouseButton::Right, true)),
            WM_RBUTTONUP => out.push(UiEvent::MouseButton(MouseButton::Right, false)),
            WM_MBUTTONDOWN | WM_MBUTTONDBLCLK => out.push(UiEvent::MouseButton(MouseButton::Middle, true)),
            WM_MBUTTONUP => out.push(UiEvent::MouseButton(MouseButton::Middle, false)),
            WM_XBUTTONDOWN | WM_XBUTTONDBLCLK | WM_XBUTTONUP => {
                let b = if (wparam >> 16) & 0xFFFF == 1 {
                    MouseButton::Extra1
                } else {
                    MouseButton::Extra2
                };
                out.push(UiEvent::MouseButton(b, msg != WM_XBUTTONUP));
            }
            WM_MOUSEWHEEL => out.push(UiEvent::Wheel([0.0, f32::from(hi_i16(wparam as isize)) / WHEEL_DELTA])),
            WM_MOUSEHWHEEL => out.push(UiEvent::Wheel([-f32::from(hi_i16(wparam as isize)) / WHEEL_DELTA, 0.0])),
            WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP => {
                let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
                let extended = (lparam as usize >> 24) & 1 == 1;
                let scan = ((lparam as usize >> 16) & 0xFF) as u32;
                // Modifiers first, so that a chord such as Ctrl+A sees Ctrl already down.
                let mut m = self.mods;
                match wparam {
                    VK_CONTROL | 0xA2 | 0xA3 => m.ctrl = down,
                    VK_SHIFT | 0xA0 | 0xA1 => m.shift = down,
                    VK_MENU | 0xA4 | 0xA5 => m.alt = down,
                    VK_LWIN | VK_RWIN => m.win = down,
                    _ => {}
                }
                self.set_modifiers(m, out);
                if let Some(k) = map_vk(wparam, extended, scan) {
                    out.push(UiEvent::Key(k, down));
                }
            }
            WM_CHAR => {
                let unit = (wparam & 0xFFFF) as u16;
                if (0xD800..0xDC00).contains(&unit) {
                    self.high_surrogate = Some(unit);
                } else if (0xDC00..0xE000).contains(&unit) {
                    if let Some(hi) = self.high_surrogate.take() {
                        if let Some(Ok(c)) = char::decode_utf16([hi, unit]).next() {
                            out.push(UiEvent::Char(c));
                        }
                    }
                } else {
                    self.high_surrogate = None;
                    // Control characters (Enter, Backspace, Tab, Ctrl+letter) arrive as key events.
                    if let Some(c) = char::from_u32(u32::from(unit)).filter(|c| !c.is_control()) {
                        out.push(UiEvent::Char(c));
                    }
                }
            }
            WM_SETFOCUS => out.push(UiEvent::Focus(true)),
            WM_KILLFOCUS => {
                // Keys released while another window has the focus never reach us: release the
                // modifiers (ImGui clears its own key state on a focus loss).
                self.set_modifiers(Modifiers::default(), out);
                self.high_surrogate = None;
                out.push(UiEvent::Focus(false));
            }
            WM_DPICHANGED => out.push(UiEvent::DpiScale(dpi_scale(((wparam >> 16) & 0xFFFF) as u32))),
            _ => return false,
        }
        true
    }
}

/// Pushes one event into the ImGui IO. `DpiScale` is not an IO event and is ignored here.
pub fn apply(io: &mut Io, e: UiEvent) {
    match e {
        UiEvent::MousePos(p) => io.add_mouse_pos_event(p),
        UiEvent::MouseButton(b, down) => io.add_mouse_button_event(b, down),
        UiEvent::Wheel(w) => io.add_mouse_wheel_event(w),
        UiEvent::Key(k, down) => io.add_key_event(k, down),
        UiEvent::Char(c) => io.add_input_character(c),
        UiEvent::Focus(f) => io.add_focus_event(f),
        UiEvent::DpiScale(_) => {}
    }
}

/// `LPARAM` of a mouse message at client position (`x`, `y`) (test and self-test helper).
pub fn mouse_lparam(x: i16, y: i16) -> isize {
    ((y as u16 as usize) << 16 | x as u16 as usize) as isize
}

/// `LPARAM` of a key message with the given scan code and extended flag (repeat count 1).
pub fn key_lparam(scan: u32, extended: bool, up: bool) -> isize {
    let mut v = 1usize | ((scan as usize & 0xFF) << 16);
    if extended {
        v |= 1 << 24;
    }
    if up {
        v |= (1 << 30) | (1 << 31);
    }
    v as isize
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(input: &mut Win32Input, msg: u32, wparam: usize, lparam: isize) -> Vec<UiEvent> {
        let mut out = Vec::new();
        assert!(
            input.translate(msg, wparam, lparam, &mut out),
            "message {msg:#x} not handled"
        );
        out
    }

    #[test]
    fn mouse_position_buttons_and_negative_coordinates() {
        let mut i = Win32Input::new();
        assert_eq!(
            run(&mut i, WM_MOUSEMOVE, 0, mouse_lparam(123, 45)),
            [UiEvent::MousePos([123.0, 45.0])]
        );
        // Coordinates are signed (captured mouse left of the client area).
        assert_eq!(
            run(&mut i, WM_MOUSEMOVE, 0, mouse_lparam(-5, -7)),
            [UiEvent::MousePos([-5.0, -7.0])]
        );
        assert_eq!(
            run(&mut i, WM_LBUTTONDOWN, 1, 0),
            [UiEvent::MouseButton(MouseButton::Left, true)]
        );
        assert_eq!(
            run(&mut i, WM_LBUTTONDBLCLK, 1, 0),
            [UiEvent::MouseButton(MouseButton::Left, true)]
        );
        assert_eq!(
            run(&mut i, WM_LBUTTONUP, 0, 0),
            [UiEvent::MouseButton(MouseButton::Left, false)]
        );
        assert_eq!(
            run(&mut i, WM_RBUTTONDOWN, 2, 0),
            [UiEvent::MouseButton(MouseButton::Right, true)]
        );
        assert_eq!(
            run(&mut i, WM_MBUTTONUP, 0, 0),
            [UiEvent::MouseButton(MouseButton::Middle, false)]
        );
        assert_eq!(
            run(&mut i, WM_XBUTTONDOWN, 1 << 16, 0),
            [UiEvent::MouseButton(MouseButton::Extra1, true)]
        );
        assert_eq!(
            run(&mut i, WM_XBUTTONUP, 2 << 16, 0),
            [UiEvent::MouseButton(MouseButton::Extra2, false)]
        );
        assert_eq!(
            run(&mut i, WM_MOUSELEAVE, 0, 0),
            [UiEvent::MousePos([-f32::MAX, -f32::MAX])]
        );
    }

    #[test]
    fn wheel_sign_and_scale() {
        let mut i = Win32Input::new();
        let up = (120usize) << 16;
        let down = ((-240i16) as u16 as usize) << 16;
        assert_eq!(run(&mut i, WM_MOUSEWHEEL, up, 0), [UiEvent::Wheel([0.0, 1.0])]);
        assert_eq!(run(&mut i, WM_MOUSEWHEEL, down, 0), [UiEvent::Wheel([0.0, -2.0])]);
        // Horizontal: positive delta (tilt right) scrolls right, which ImGui expresses as -x.
        assert_eq!(run(&mut i, WM_MOUSEHWHEEL, up, 0), [UiEvent::Wheel([-1.0, 0.0])]);
        // Fine-grained wheels send fractions of WHEEL_DELTA.
        assert_eq!(run(&mut i, WM_MOUSEWHEEL, 30 << 16, 0), [UiEvent::Wheel([0.0, 0.25])]);
    }

    #[test]
    fn editing_and_navigation_keys() {
        let mut i = Win32Input::new();
        for (vk, scan, ext, key) in [
            (0x09, 0x0F, false, Key::Tab),
            (0x0D, 0x1C, false, Key::Enter),
            (0x0D, 0x1C, true, Key::KeypadEnter),
            (0x08, 0x0E, false, Key::Backspace),
            (0x25, 0x4B, true, Key::LeftArrow),
            (0x27, 0x4D, true, Key::RightArrow),
            (0x26, 0x48, true, Key::UpArrow),
            (0x28, 0x50, true, Key::DownArrow),
            (0x2E, 0x53, true, Key::Delete),
            (0x24, 0x47, true, Key::Home),
            (0x23, 0x4F, true, Key::End),
            (0x1B, 0x01, false, Key::Escape),
            (0x41, 0x1E, false, Key::A),
            (0x5A, 0x2C, false, Key::Z),
            (0x35, 0x06, false, Key::Key5),
            (0x7B, 0x58, false, Key::F12),
            (0x67, 0x47, false, Key::Keypad7),
            (0xBD, 0x0C, false, Key::Minus),
        ] {
            assert_eq!(
                run(&mut i, WM_KEYDOWN, vk, key_lparam(scan, ext, false)),
                [UiEvent::Key(key, true)],
                "vk {vk:#x} down"
            );
            assert_eq!(
                run(&mut i, WM_KEYUP, vk, key_lparam(scan, ext, true)),
                [UiEvent::Key(key, false)],
                "vk {vk:#x} up"
            );
        }
        // Unknown keys are consumed but produce no event.
        assert!(run(&mut i, WM_KEYDOWN, 0xFF, key_lparam(0, false, false)).is_empty());
    }

    #[test]
    fn modifiers_left_right_and_mod_events() {
        let mut i = Win32Input::new();
        // Left Ctrl down: ModCtrl then the key itself.
        assert_eq!(
            run(&mut i, WM_KEYDOWN, VK_CONTROL, key_lparam(0x1D, false, false)),
            [UiEvent::Key(Key::ModCtrl, true), UiEvent::Key(Key::LeftCtrl, true)]
        );
        // Auto-repeat of a held modifier: no second ModCtrl event.
        assert_eq!(
            run(&mut i, WM_KEYDOWN, VK_CONTROL, key_lparam(0x1D, false, false)),
            [UiEvent::Key(Key::LeftCtrl, true)]
        );
        assert_eq!(
            i.modifiers(),
            Modifiers {
                ctrl: true,
                ..Modifiers::default()
            }
        );
        // Chord: Ctrl+A.
        assert_eq!(
            run(&mut i, WM_KEYDOWN, 0x41, key_lparam(0x1E, false, false)),
            [UiEvent::Key(Key::A, true)]
        );
        assert_eq!(
            run(&mut i, WM_KEYUP, VK_CONTROL, key_lparam(0x1D, false, true)),
            [UiEvent::Key(Key::ModCtrl, false), UiEvent::Key(Key::LeftCtrl, false)]
        );
        // Right variants from the extended bit and the right-Shift scan code.
        assert_eq!(
            run(&mut i, WM_KEYDOWN, VK_CONTROL, key_lparam(0x1D, true, false)),
            [UiEvent::Key(Key::ModCtrl, true), UiEvent::Key(Key::RightCtrl, true)]
        );
        assert_eq!(
            run(&mut i, WM_KEYDOWN, VK_SHIFT, key_lparam(SCAN_RSHIFT, false, false)),
            [UiEvent::Key(Key::ModShift, true), UiEvent::Key(Key::RightShift, true)]
        );
        assert_eq!(
            run(&mut i, WM_KEYDOWN, VK_SHIFT, key_lparam(0x2A, false, false)),
            [UiEvent::Key(Key::LeftShift, true)]
        );
        // Alt arrives as WM_SYSKEYDOWN.
        assert_eq!(
            run(&mut i, WM_SYSKEYDOWN, VK_MENU, key_lparam(0x38, false, false)),
            [UiEvent::Key(Key::ModAlt, true), UiEvent::Key(Key::LeftAlt, true)]
        );
        assert_eq!(
            run(&mut i, WM_SYSKEYUP, VK_MENU, key_lparam(0x38, true, true)),
            [UiEvent::Key(Key::ModAlt, false), UiEvent::Key(Key::RightAlt, false)]
        );
        assert_eq!(
            run(&mut i, WM_KEYDOWN, VK_LWIN, key_lparam(0x5B, true, false)),
            [UiEvent::Key(Key::ModSuper, true), UiEvent::Key(Key::LeftSuper, true)]
        );
        // Focus loss releases every modifier still down, then reports the focus change.
        let out = run(&mut i, WM_KILLFOCUS, 0, 0);
        assert_eq!(
            out,
            [
                UiEvent::Key(Key::ModCtrl, false),
                UiEvent::Key(Key::ModShift, false),
                UiEvent::Key(Key::ModSuper, false),
                UiEvent::Focus(false)
            ]
        );
        assert_eq!(i.modifiers(), Modifiers::default());
        assert_eq!(run(&mut i, WM_SETFOCUS, 0, 0), [UiEvent::Focus(true)]);
        // Held keys found at focus gain.
        let mut out = Vec::new();
        i.set_modifiers(
            Modifiers {
                shift: true,
                ..Modifiers::default()
            },
            &mut out,
        );
        assert_eq!(out, [UiEvent::Key(Key::ModShift, true)]);
    }

    #[test]
    fn characters_utf16_and_control_filter() {
        let mut i = Win32Input::new();
        assert_eq!(run(&mut i, WM_CHAR, 'a' as usize, 1), [UiEvent::Char('a')]);
        assert_eq!(run(&mut i, WM_CHAR, 0xE9, 1), [UiEvent::Char('\u{e9}')]);
        // Enter, Backspace, Tab, Ctrl+A arrive as WM_CHAR control codes: dropped (keys carry them).
        for c in [0x0D, 0x08, 0x09, 0x01, 0x7F] {
            assert!(run(&mut i, WM_CHAR, c, 1).is_empty(), "control {c:#x}");
        }
        // U+1F600 as a surrogate pair.
        assert!(run(&mut i, WM_CHAR, 0xD83D, 1).is_empty());
        assert_eq!(run(&mut i, WM_CHAR, 0xDE00, 1), [UiEvent::Char('\u{1F600}')]);
        // A lone low surrogate is dropped.
        assert!(run(&mut i, WM_CHAR, 0xDE00, 1).is_empty());
    }

    #[test]
    fn dpi_change_and_unrelated_messages() {
        let mut i = Win32Input::new();
        assert_eq!(dpi_scale(96), 1.0);
        assert_eq!(dpi_scale(144), 1.5);
        assert_eq!(
            run(&mut i, WM_DPICHANGED, (120 << 16) | 120, 0),
            [UiEvent::DpiScale(1.25)]
        );
        let mut out = Vec::new();
        assert!(!i.translate(0x000F /* WM_PAINT */, 0, 0, &mut out));
        assert!(out.is_empty());
    }
}
