//! Global hotkeys (T-54, PLAN D-17, QUESTIONS OUT-5): chords, actions, settings parsing and the registration
//! plan. Pure: no OS call is made here. The `RegisterHotKey` calls are made by `output.rs` through the
//! [`Registrar`] trait, so that the fallback logic is unit tested with a fake registrar.
//!
//! `RegisterHotKey` only (no low-level hook): a registered chord is swallowed system-wide, so the captured game
//! loses it while phoxium runs (documented in the README), but the foreground window never changes.
//! Defaults: Quit `Ctrl+Alt+End`, fullscreen `F11`, settings menu `F10` (F12 is registered by another program on
//! the reference PC, error 1409), overlay `Ctrl+Alt+F9`, A/B bypass `Ctrl+Alt+F7`, next and previous preset
//! `Ctrl+Alt+PageDown` and `Ctrl+Alt+PageUp`, click-through `Ctrl+Alt+F6`. Every action, Quit included, can be rebound; Quit is never left
//! unbound by a rebind or a reset (the old chord is kept when the new one cannot be registered).
//!
//! AltGr (review D-01): Windows sends AltGr as LeftCtrl+RightAlt, so a `Ctrl+Alt+<key>` hotkey also fires on
//! AltGr+key and swallows the character that key types on European layouts (`@` is AltGr+Q in German, `ó` is
//! AltGr+O in Polish). The defaults therefore use no character key with Ctrl+Alt: only function and navigation
//! keys, which type nothing under AltGr on any layout. They also avoid the chords of the Intel graphics hotkeys
//! (Ctrl+Alt+F1, F3, F4, F8, F11, F12 and the arrows, which rotate the screen). Earlier builds used
//! `Ctrl+Alt+Q`, `Ctrl+Alt+O` and `Ctrl+Alt+B` ([`Action::old_default_text`], migrated by `settings`). A chord
//! the user picks with Ctrl+Alt and a character key is checked against the installed layouts ([`altgr_conflict`]).

use std::{collections::BTreeMap, fmt, str::FromStr};

/// Modifier set, with the values of the Win32 `MOD_*` flags (`MOD_ALT` 1, `MOD_CONTROL` 2, `MOD_SHIFT` 4, `MOD_WIN` 8).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Default)]
pub struct Mods(u8);

impl Mods {
    pub const NONE: Mods = Mods(0);
    pub const ALT: Mods = Mods(1);
    pub const CTRL: Mods = Mods(2);
    pub const SHIFT: Mods = Mods(4);
    pub const WIN: Mods = Mods(8);

    pub fn contains(self, other: Mods) -> bool {
        self.0 & other.0 == other.0
    }

    /// The Win32 `MOD_*` flags (without `MOD_NOREPEAT`).
    pub fn bits(self) -> u32 {
        u32::from(self.0)
    }

    /// From Win32 `MOD_*` flags (the low word of `WM_HOTKEY`'s `LPARAM`); other bits are ignored.
    pub fn from_bits(bits: u32) -> Mods {
        Mods((bits & 0xF) as u8)
    }
}

impl std::ops::BitOr for Mods {
    type Output = Mods;
    fn bitor(self, rhs: Mods) -> Mods {
        Mods(self.0 | rhs.0)
    }
}

/// Keys that are not a letter, a digit or a function key: canonical name and virtual-key code.
const NAMED_KEYS: &[(&str, u16)] = &[
    ("PageUp", 0x21),
    ("PageDown", 0x22),
    ("End", 0x23),
    ("Home", 0x24),
    ("Left", 0x25),
    ("Up", 0x26),
    ("Right", 0x27),
    ("Down", 0x28),
    ("Insert", 0x2D),
    ("Delete", 0x2E),
    ("Space", 0x20),
    ("Pause", 0x13),
    ("ScrollLock", 0x91),
    ("PrintScreen", 0x2C),
];

/// Accepted spellings besides the canonical names (compared case-insensitively).
const KEY_ALIASES: &[(&str, u16)] = &[("PgUp", 0x21), ("PgDn", 0x22), ("Ins", 0x2D), ("Del", 0x2E)];

const VK_F1: u16 = 0x70;

fn key_from_name(name: &str) -> Option<u16> {
    let lower = name.to_ascii_lowercase();
    if let Some(digits) = lower.strip_prefix('f') {
        if let Some(n) = digits
            .parse::<u16>()
            .ok()
            .filter(|_| digits.bytes().all(|b| b.is_ascii_digit()))
        {
            if (1..=24).contains(&n) {
                return Some(VK_F1 + n - 1);
            }
        }
    }
    let mut chars = lower.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if c.is_ascii_alphabetic() || c.is_ascii_digit() {
            return Some(c.to_ascii_uppercase() as u16);
        }
    }
    NAMED_KEYS
        .iter()
        .chain(KEY_ALIASES)
        .find(|(n, _)| n.eq_ignore_ascii_case(&lower))
        .map(|&(_, vk)| vk)
}

fn key_name(vk: u16) -> String {
    match vk {
        0x70..=0x87 => format!("F{}", vk - VK_F1 + 1),
        0x30..=0x39 | 0x41..=0x5A => char::from(vk as u8).to_string(),
        _ => NAMED_KEYS
            .iter()
            .find(|&&(_, v)| v == vk)
            .map_or_else(|| format!("VK_{vk:#04X}"), |&(n, _)| n.to_string()),
    }
}

fn is_function_key(vk: u16) -> bool {
    (0x70..=0x87).contains(&vk)
}

/// Canonical order of the modifiers in the text of a chord.
const MOD_ORDER: [Mods; 4] = [Mods::CTRL, Mods::ALT, Mods::SHIFT, Mods::WIN];

fn mod_name(m: Mods) -> &'static str {
    match m {
        Mods::CTRL => "Ctrl",
        Mods::ALT => "Alt",
        Mods::SHIFT => "Shift",
        _ => "Win",
    }
}

fn mod_from_name(name: &str) -> Option<Mods> {
    match name.to_ascii_lowercase().as_str() {
        "ctrl" | "control" => Some(Mods::CTRL),
        "alt" => Some(Mods::ALT),
        "shift" => Some(Mods::SHIFT),
        "win" | "windows" | "super" => Some(Mods::WIN),
        _ => None,
    }
}

/// Why a chord text was rejected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChordError {
    Empty,
    /// An empty part, as in `Ctrl++Q` or a trailing `+`.
    EmptyPart,
    UnknownKey(String),
    NotAModifier(String),
    DuplicateModifier(String),
    /// Only modifiers, as in `Ctrl+Alt`.
    MissingKey,
    /// A chord without Ctrl, Alt or Win on a key other than a function key would swallow normal typing.
    NeedsModifier(String),
}

impl fmt::Display for ChordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChordError::Empty => write!(f, "is empty"),
            ChordError::EmptyPart => write!(f, "has an empty part (expected Mod+Mod+Key)"),
            ChordError::UnknownKey(k) => write!(f, "has an unknown key '{k}' (F1 to F24, A to Z, 0 to 9, PageUp, PageDown, Home, End, Insert, Delete, arrows, Space, Pause, ScrollLock, PrintScreen)"),
            ChordError::NotAModifier(m) => write!(f, "has '{m}' where a modifier (Ctrl, Alt, Shift, Win) is expected"),
            ChordError::DuplicateModifier(m) => write!(f, "repeats the modifier {m}"),
            ChordError::MissingKey => write!(f, "has no key after the modifiers"),
            ChordError::NeedsModifier(k) => write!(f, "is the bare key {k}: only function keys may be used without Ctrl, Alt or Win"),
        }
    }
}

impl std::error::Error for ChordError {}

/// A global hotkey: modifiers and one key.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Chord {
    pub mods: Mods,
    /// Windows virtual-key code.
    pub vk: u16,
}

impl Chord {
    /// A chord, checked against the rule that only function keys may go without Ctrl, Alt or Win.
    pub fn new(mods: Mods, vk: u16) -> Result<Chord, ChordError> {
        let strong = mods.contains(Mods::CTRL) || mods.contains(Mods::ALT) || mods.contains(Mods::WIN);
        if !strong && !is_function_key(vk) {
            return Err(ChordError::NeedsModifier(key_name(vk)));
        }
        Ok(Chord { mods, vk })
    }
}

impl fmt::Display for Chord {
    /// The canonical text: `Ctrl+Alt+Shift+Win+Key`, as [`Chord::from_str`] reads it back.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for m in MOD_ORDER {
            if self.mods.contains(m) {
                write!(f, "{}+", mod_name(m))?;
            }
        }
        write!(f, "{}", key_name(self.vk))
    }
}

impl FromStr for Chord {
    type Err = ChordError;

    /// Reads `F10`, `Ctrl+Alt+F12`, `ctrl+alt+pagedown` (case-insensitive, `PgDn` and `Del` accepted).
    fn from_str(text: &str) -> Result<Chord, ChordError> {
        let text = text.trim();
        if text.is_empty() {
            return Err(ChordError::Empty);
        }
        let parts: Vec<&str> = text.split('+').map(str::trim).collect();
        if parts.iter().any(|p| p.is_empty()) {
            return Err(ChordError::EmptyPart);
        }
        let (key, modifiers) = parts.split_last().expect("split gives at least one part");
        let mut mods = Mods::NONE;
        for m in modifiers {
            let bit = mod_from_name(m).ok_or_else(|| ChordError::NotAModifier((*m).to_string()))?;
            if mods.contains(bit) {
                return Err(ChordError::DuplicateModifier(mod_name(bit).to_string()));
            }
            mods = mods | bit;
        }
        let vk = match key_from_name(key) {
            Some(vk) => vk,
            None if mod_from_name(key).is_some() => return Err(ChordError::MissingKey),
            None => return Err(ChordError::UnknownKey((*key).to_string())),
        };
        Chord::new(mods, vk)
    }
}

/// What a hotkey does. The discriminant plus one is the `RegisterHotKey` id.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub enum Action {
    Quit,
    ToggleFullscreen,
    ToggleMenu,
    ToggleOverlay,
    /// A/B: full bypass of the CRT chain (all chain slots disabled), the picture goes through source placement only.
    ToggleBypass,
    NextPreset,
    PreviousPreset,
    /// Click-through of the output on or off (`[global.output] click_through`).
    ToggleClickThrough,
    /// Saves a snapshot of the output (the picture without the menu) as a PNG in the pictures folder.
    Snapshot,
    /// Hold to bypass: the CRT chain is off while the key is held down and back on at its release (the toggle
    /// [`Action::ToggleBypass`] is unaffected). A registered hotkey reports presses only, so the release is polled
    /// (`output::key_down`).
    HoldBypass,
}

impl Action {
    /// Every action, in registration order (Quit first: it is the escape path).
    pub const ALL: [Action; 10] = [
        Action::Quit,
        Action::ToggleFullscreen,
        Action::ToggleMenu,
        Action::ToggleOverlay,
        Action::ToggleBypass,
        Action::NextPreset,
        Action::PreviousPreset,
        Action::ToggleClickThrough,
        Action::Snapshot,
        Action::HoldBypass,
    ];

    /// The key of the action in `[global.hotkeys]` of `settings.toml`.
    pub fn id(self) -> &'static str {
        match self {
            Action::Quit => "quit",
            Action::ToggleFullscreen => "toggle_fullscreen",
            Action::ToggleMenu => "toggle_menu",
            Action::ToggleOverlay => "toggle_overlay",
            Action::ToggleBypass => "toggle_bypass",
            Action::NextPreset => "next_preset",
            Action::PreviousPreset => "previous_preset",
            Action::ToggleClickThrough => "toggle_click_through",
            Action::Snapshot => "snapshot",
            Action::HoldBypass => "hold_bypass",
        }
    }

    pub fn from_id(id: &str) -> Option<Action> {
        Action::ALL.into_iter().find(|a| a.id() == id)
    }

    /// The default chord text (the same as `settings::DEFAULT_HOTKEYS`, checked by a test).
    pub fn default_text(self) -> &'static str {
        match self {
            Action::Quit => "Ctrl+Alt+End",
            Action::ToggleFullscreen => "F11",
            Action::ToggleMenu => "F10",
            Action::ToggleOverlay => "Ctrl+Alt+F9",
            Action::ToggleBypass => "Ctrl+Alt+F7",
            Action::NextPreset => "Ctrl+Alt+PageDown",
            Action::PreviousPreset => "Ctrl+Alt+PageUp",
            Action::ToggleClickThrough => "Ctrl+Alt+F6",
            Action::Snapshot => "Ctrl+Alt+F2",
            Action::HoldBypass => "Ctrl+Alt+Insert",
        }
    }

    /// The default chord of earlier builds, for the actions whose default changed (review D-01: these fired on
    /// AltGr). A settings file written before the change holds them explicitly; `settings` upgrades them.
    pub fn old_default_text(self) -> Option<&'static str> {
        match self {
            Action::Quit => Some("Ctrl+Alt+Q"),
            Action::ToggleOverlay => Some("Ctrl+Alt+O"),
            Action::ToggleBypass => Some("Ctrl+Alt+B"),
            _ => None,
        }
    }

    pub fn default_chord(self) -> Chord {
        self.default_text().parse().expect("the default chords are valid")
    }

    /// Chords tried in this order when the wanted one cannot be registered (another program owns it).
    /// The menu list ends with `Ctrl+Alt+F12` (D-17). A fullscreen output refuses to start without a Quit chord.
    pub fn fallbacks(self) -> &'static [&'static str] {
        match self {
            // The default first: a rebound Quit whose chord is taken at start goes back to Ctrl+Alt+End.
            Action::Quit => &["Ctrl+Alt+End", "Ctrl+Alt+Shift+End"],
            Action::ToggleFullscreen => &["Ctrl+Alt+F11"],
            Action::ToggleMenu => &["F10", "Ctrl+Alt+F10", "Ctrl+Alt+F12"],
            Action::ToggleOverlay => &["Ctrl+Alt+Shift+F9"],
            Action::ToggleBypass => &["Ctrl+Alt+Shift+F7"],
            Action::NextPreset => &["Ctrl+Alt+Shift+PageDown"],
            Action::PreviousPreset => &["Ctrl+Alt+Shift+PageUp"],
            Action::ToggleClickThrough => &["Ctrl+Alt+Shift+F6"],
            Action::Snapshot => &["Ctrl+Alt+Shift+F2"],
            Action::HoldBypass => &["Ctrl+Alt+Shift+Insert"],
        }
    }

    /// A short phrase for messages.
    pub fn describe(self) -> &'static str {
        match self {
            Action::Quit => "quit",
            Action::ToggleFullscreen => "fullscreen / windowed",
            Action::ToggleMenu => "settings menu",
            Action::ToggleOverlay => "overlay",
            Action::ToggleBypass => "A/B bypass",
            Action::NextPreset => "next preset",
            Action::PreviousPreset => "previous preset",
            Action::ToggleClickThrough => "click-through",
            Action::Snapshot => "snapshot",
            Action::HoldBypass => "hold to bypass",
        }
    }

    /// The `RegisterHotKey` id of the action.
    pub fn hotkey_id(self) -> i32 {
        self as i32 + 1
    }

    /// The action of a `RegisterHotKey` id; [`MENU_FALLBACK_ID`] is the settings menu too.
    pub fn from_hotkey_id(id: i32) -> Option<Action> {
        if id == MENU_FALLBACK_ID {
            return Some(Action::ToggleMenu);
        }
        Action::ALL.into_iter().find(|a| a.hotkey_id() == id)
    }
}

/// The chord wanted for each action, after validation. An action is missing only when its chord and its
/// default both collide with another action's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bindings {
    chords: BTreeMap<Action, Chord>,
}

impl Default for Bindings {
    fn default() -> Self {
        Bindings {
            chords: Action::ALL.into_iter().map(|a| (a, a.default_chord())).collect(),
        }
    }
}

impl Bindings {
    pub fn get(&self, action: Action) -> Option<Chord> {
        self.chords.get(&action).copied()
    }

    pub fn iter(&self) -> impl Iterator<Item = (Action, Chord)> + '_ {
        self.chords.iter().map(|(a, c)| (*a, *c))
    }

    /// Reads `[global.hotkeys]` (action id to chord text). Every invalid entry gives a warning and the
    /// default chord; a chord already taken by an earlier action (in [`Action::ALL`] order) is rejected, the
    /// default is used instead, and the action stays unbound when the default is taken as well; unknown action
    /// ids are ignored with a warning. Quit comes first, so it always gets its chord (or its default). Returns the
    /// bindings and the warnings to show.
    pub fn from_settings(map: &BTreeMap<String, String>) -> (Bindings, Vec<String>) {
        let mut warnings = Vec::new();
        for key in map.keys() {
            if Action::from_id(key).is_none() {
                warnings.push(format!("hotkey '{key}': unknown action ignored"));
            }
        }
        let mut chords: BTreeMap<Action, Chord> = BTreeMap::new();
        for action in Action::ALL {
            let default = action.default_chord();
            let id = action.id();
            let wanted = match map.get(id) {
                None => default,
                Some(text) => match text.parse::<Chord>() {
                    Ok(c) => c,
                    Err(e) => {
                        warnings.push(format!("hotkey {id}: '{text}' {e}; default {default} used"));
                        default
                    }
                },
            };
            let owner = |c: &Chord| chords.iter().find(|(_, o)| *o == c).map(|(a, _)| *a);
            match owner(&wanted) {
                None => {
                    chords.insert(action, wanted);
                }
                Some(other) => {
                    let prefix = format!("hotkey {id}: {wanted} is already used by {}", other.id());
                    if wanted != default && owner(&default).is_none() {
                        warnings.push(format!("{prefix}; default {default} used"));
                        chords.insert(action, default);
                    } else {
                        warnings.push(format!("{prefix}; the action is left unbound"));
                    }
                }
            }
        }
        (Bindings { chords }, warnings)
    }

    /// The chords as `settings.toml` text, by action id.
    pub fn to_settings(&self) -> BTreeMap<String, String> {
        self.chords
            .iter()
            .map(|(a, c)| (a.id().to_string(), c.to_string()))
            .collect()
    }
}

/// The chords to try for one action, in order: the wanted one, then the fallbacks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attempt {
    pub action: Action,
    pub candidates: Vec<Chord>,
}

/// The registration plan, in registration order: one [`Attempt`] per bound action, with the action's fallbacks
/// after the wanted chord. A fallback that equals a candidate already listed or the wanted chord of another
/// action is left out. Pure.
pub fn plan_registration(bindings: &Bindings) -> Vec<Attempt> {
    let wanted: Vec<Chord> = bindings.iter().map(|(_, c)| c).collect();
    bindings
        .iter()
        .map(|(action, chord)| {
            let mut candidates = vec![chord];
            for text in action.fallbacks() {
                let c: Chord = text.parse().expect("the fallback chords are valid");
                if !candidates.contains(&c) && !wanted.contains(&c) {
                    candidates.push(c);
                }
            }
            Attempt { action, candidates }
        })
        .collect()
}

/// `ERROR_HOTKEY_ALREADY_REGISTERED`.
pub const ERROR_HOTKEY_ALREADY_REGISTERED: u32 = 1409;

/// The Win32 error code inside an HRESULT (`HRESULT_FROM_WIN32`), or the value itself when it is not one.
pub fn win32_code(hresult: i32) -> u32 {
    let v = hresult as u32;
    if v >> 16 == 0x8007 {
        v & 0xFFFF
    } else {
        v
    }
}

/// A readable text for the codes `RegisterHotKey` returns in practice.
pub fn describe_win32(code: u32) -> String {
    match code {
        ERROR_HOTKEY_ALREADY_REGISTERED => {
            format!("hot key is already registered by another program, Win32 error {code}")
        }
        1400 => format!("invalid window handle, Win32 error {code}"),
        87 => format!("invalid parameter, Win32 error {code}"),
        _ => format!("Win32 error {code}"),
    }
}

/// Registers and unregisters chords with the system (`RegisterHotKey` in `output.rs`, a fake in the tests).
pub trait Registrar {
    /// Registers `chord` under `id`; the error is the Win32 error code.
    fn register(&mut self, id: i32, chord: &Chord) -> Result<(), u32>;
    fn unregister(&mut self, id: i32);
}

/// What happened for one action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub action: Action,
    /// The chord asked for (the first candidate).
    pub wanted: Chord,
    /// The chord that is registered, `None` when every candidate failed.
    pub bound: Option<Chord>,
    /// The candidates that failed, in the order tried, with the Win32 error code.
    pub failures: Vec<(Chord, u32)>,
}

impl Outcome {
    /// The console line for a problem: names the chord and the error code. `None` when the wanted chord is bound.
    pub fn message(&self) -> Option<String> {
        if self.failures.is_empty() {
            return None;
        }
        let list = self
            .failures
            .iter()
            .map(|(c, e)| format!("{c} ({})", describe_win32(*e)))
            .collect::<Vec<_>>()
            .join(", ");
        let id = self.action.id();
        Some(match self.bound {
            Some(b) => format!("hotkey {id}: cannot register {list}; using {b} instead"),
            None => format!(
                "hotkey {id}: cannot register {list}; the {} hotkey is unavailable (set another chord in [global.hotkeys])",
                self.action.describe()
            ),
        })
    }
}

/// The result of [`register_all`]: the state the UI can display.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Registration {
    pub outcomes: Vec<Outcome>,
    /// The settings menu's safety chord ([`update_menu_fallback`]): registered under [`MENU_FALLBACK_ID`] while
    /// the menu key in force is not its default, so the menu can always be opened.
    pub menu_fallback: Option<Chord>,
}

impl Registration {
    /// The chord registered for `action`.
    pub fn bound(&self, action: Action) -> Option<Chord> {
        self.outcomes.iter().find(|o| o.action == action).and_then(|o| o.bound)
    }

    /// What happened for `action`.
    pub fn outcome(&self, action: Action) -> Option<&Outcome> {
        self.outcomes.iter().find(|o| o.action == action)
    }

    /// The chords asked for, as `[global.hotkeys]` text by action id (the default for an action not planned).
    pub fn to_settings(&self) -> BTreeMap<String, String> {
        Action::ALL
            .into_iter()
            .map(|a| {
                let text = self
                    .outcome(a)
                    .map_or_else(|| a.default_text().to_string(), |o| o.wanted.to_string());
                (a.id().to_string(), text)
            })
            .collect()
    }

    /// Whether `chord` may become the chord of `action`: a chord another action asks for or holds is refused.
    /// The message is for the menu.
    pub fn check(&self, action: Action, chord: Chord) -> Result<(), String> {
        match self
            .outcomes
            .iter()
            .find(|o| o.action != action && (o.bound == Some(chord) || o.wanted == chord))
        {
            Some(o) => Err(format!("{chord} is already used for {}.", o.action.describe())),
            None => Ok(()),
        }
    }

    /// The line the menu shows about the menu's safety chord, when one is registered.
    pub fn fallback_note(&self) -> Option<String> {
        self.menu_fallback
            .map(|c| format!("{c} also opens the menu (kept as a fallback while the menu key is changed)."))
    }

    /// One message per action whose wanted chord could not be registered (empty when all went through).
    pub fn problems(&self) -> Vec<String> {
        self.outcomes.iter().filter_map(Outcome::message).collect()
    }

    /// The state flag for the UI: some wanted chord is not the one in force.
    pub fn has_problems(&self) -> bool {
        self.outcomes.iter().any(|o| !o.failures.is_empty())
    }

    /// `quit Ctrl+Alt+End, fullscreen / windowed F11, ...` for the console and the menu help.
    pub fn summary(&self) -> String {
        self.outcomes
            .iter()
            .map(|o| match o.bound {
                Some(c) => format!("{} {c}", o.action.describe()),
                None => format!("{} unavailable", o.action.describe()),
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Unregisters every chord that is registered (the menu's safety chord included).
    pub fn unregister_all(&self, reg: &mut dyn Registrar) {
        for o in self.outcomes.iter().filter(|o| o.bound.is_some()) {
            reg.unregister(o.action.hotkey_id());
        }
        if self.menu_fallback.is_some() {
            reg.unregister(MENU_FALLBACK_ID);
        }
    }
}

// ---------------------------------------------------------------------------------------------------------
// Rebinding from the settings menu (owner request 2026-11-20).

/// `RegisterHotKey` id of the menu's safety chord ([`update_menu_fallback`]); [`Action::from_hotkey_id`] maps it
/// to the settings menu.
pub const MENU_FALLBACK_ID: i32 = 64;

/// What a key press means while the menu waits for a new chord ([`capture_key`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Captured {
    /// A modifier alone (Ctrl, Alt, Shift, Win): keep waiting for the key.
    Waiting,
    /// Esc: stop waiting, nothing changes.
    Cancel,
    /// Backspace, or Delete without a modifier: back to the action's default chord.
    Default,
    /// A valid chord.
    Chord(Chord),
    /// A key that cannot be a hotkey (the reason, for the menu); keep waiting.
    Invalid(String),
}

/// Virtual keys that are modifiers on their own (Shift, Ctrl, Alt, left and right, the Windows keys).
fn is_modifier_key(vk: u16) -> bool {
    matches!(vk, 0x10..=0x12 | 0xA0..=0xA5 | 0x5B | 0x5C)
}

/// The meaning of key `vk` pressed with `mods` held while a new chord is awaited. Pure.
pub fn capture_key(vk: u16, mods: Mods) -> Captured {
    match vk {
        _ if is_modifier_key(vk) => Captured::Waiting,
        0x1B => Captured::Cancel,
        0x08 => Captured::Default,
        0x2E if mods == Mods::NONE => Captured::Default,
        _ => {
            let name = key_name(vk);
            if key_from_name(&name) != Some(vk) {
                return Captured::Invalid(
                    "That key cannot be a hotkey: use F1 to F24, a letter, a digit, an arrow or a named key such as \
                     Home or PageUp."
                        .to_string(),
                );
            }
            match Chord::new(mods, vk) {
                Ok(c) => Captured::Chord(c),
                Err(_) => Captured::Invalid(format!("{name} needs Ctrl, Alt or Win (only F1 to F24 work alone).")),
            }
        }
    }
}

/// The chords that may serve as the menu's safety chord, in order: none while the menu key in force is its
/// default (F10); else the documented menu fallbacks (F10, Ctrl+Alt+F10, Ctrl+Alt+F12) that are neither the menu
/// key itself nor a chord another action asks for or holds.
pub fn menu_fallback_candidates(state: &Registration) -> Vec<Chord> {
    let menu = state.bound(Action::ToggleMenu);
    if menu == Some(Action::ToggleMenu.default_chord()) {
        return Vec::new();
    }
    let others: Vec<Chord> = state
        .outcomes
        .iter()
        .filter(|o| o.action != Action::ToggleMenu)
        .flat_map(|o| [Some(o.wanted), o.bound])
        .flatten()
        .collect();
    Action::ToggleMenu
        .fallbacks()
        .iter()
        .map(|t| t.parse::<Chord>().expect("the fallback chords are valid"))
        .filter(|c| Some(*c) != menu && !others.contains(c))
        .collect()
}

/// Keeps the menu reachable (owner requirement: changing the menu key must never leave the user without a way
/// to open the menu): while the menu key in force is not its default, the first free documented fallback
/// ([`menu_fallback_candidates`]) stays registered under [`MENU_FALLBACK_ID`]. A safety chord still valid is kept;
/// one no longer needed or now taken by an action is released.
pub fn update_menu_fallback(reg: &mut dyn Registrar, state: &mut Registration) {
    let candidates = menu_fallback_candidates(state);
    if let Some(c) = state.menu_fallback {
        if candidates.contains(&c) {
            return;
        }
        reg.unregister(MENU_FALLBACK_ID);
        state.menu_fallback = None;
    }
    for c in candidates {
        if reg.register(MENU_FALLBACK_ID, &c).is_ok() {
            state.menu_fallback = Some(c);
            break;
        }
    }
}

/// Rebinds `action` to `chord` at once: checks it ([`Registration::check`]), releases the old chord, registers
/// the new one; when the system refuses it (another program owns it) the old chord is registered again and kept.
/// The menu's safety chord follows ([`update_menu_fallback`]). Returns the message for the menu: `Ok` when the
/// new chord is in force, `Err` when nothing changed (or, rarely, the old chord could not be taken back either,
/// which the message says).
pub fn rebind(
    reg: &mut dyn Registrar,
    state: &mut Registration,
    action: Action,
    chord: Chord,
) -> Result<String, String> {
    state.check(action, chord)?;
    let result = set_chord(reg, state, action, chord);
    update_menu_fallback(reg, state);
    result
}

/// [`rebind`] without the check and without the menu's safety chord. Quit is never left without a chord: when
/// neither the new chord nor the old one can be registered, its default and its fallbacks are tried.
fn set_chord(
    reg: &mut dyn Registrar,
    state: &mut Registration,
    action: Action,
    chord: Chord,
) -> Result<String, String> {
    let id = action.hotkey_id();
    let i = match state.outcomes.iter().position(|o| o.action == action) {
        Some(i) => i,
        None => {
            state.outcomes.push(Outcome {
                action,
                wanted: chord,
                bound: None,
                failures: Vec::new(),
            });
            state.outcomes.sort_by_key(|o| o.action);
            state
                .outcomes
                .iter()
                .position(|o| o.action == action)
                .expect("just added")
        }
    };
    let old = state.outcomes[i].bound;
    let what = capitalise(action.describe());
    if old == Some(chord) {
        let o = &mut state.outcomes[i];
        o.wanted = chord;
        o.failures.clear();
        return Ok(format!("{what}: {chord} (unchanged)."));
    }
    if state.menu_fallback == Some(chord) {
        // The safety chord gives way; another one is chosen below.
        reg.unregister(MENU_FALLBACK_ID);
        state.menu_fallback = None;
    }
    if old.is_some() {
        reg.unregister(id);
    }
    match reg.register(id, &chord) {
        Ok(()) => {
            state.outcomes[i] = Outcome {
                action,
                wanted: chord,
                bound: Some(chord),
                failures: Vec::new(),
            };
            Ok(format!("{what}: {chord}."))
        }
        Err(code) => {
            let why = describe_win32(code);
            match old {
                Some(o) if reg.register(id, &o).is_ok() => Err(format!("{chord} cannot be used ({why}); {o} is kept.")),
                Some(o) => {
                    state.outcomes[i].bound = None;
                    state.outcomes[i].failures = vec![(chord, code)];
                    if action == Action::Quit {
                        if let Some(safe) = rescue_quit(reg, state, &[chord, o]) {
                            return Err(format!(
                                "{chord} cannot be used ({why}), and {o} could not be taken back: Quit is on {safe} now."
                            ));
                        }
                    }
                    Err(format!(
                        "{chord} cannot be used ({why}), and {o} could not be taken back: {what} has no hotkey now."
                    ))
                }
                None if action == Action::Quit => match rescue_quit(reg, state, &[chord]) {
                    Some(safe) => Err(format!("{chord} cannot be used ({why}); Quit is on {safe}.")),
                    None => Err(format!("{chord} cannot be used ({why}).")),
                },
                None => Err(format!("{chord} cannot be used ({why}).")),
            }
        }
    }
}

/// Quit lost its chord: its default, then its fallbacks (except `tried` and the chords other actions ask for or
/// hold), until one registers. Returns the chord now bound.
fn rescue_quit(reg: &mut dyn Registrar, state: &mut Registration, tried: &[Chord]) -> Option<Chord> {
    let others: Vec<Chord> = state
        .outcomes
        .iter()
        .filter(|o| o.action != Action::Quit)
        .flat_map(|o| [Some(o.wanted), o.bound])
        .flatten()
        .chain(state.menu_fallback)
        .collect();
    // The fallbacks start with the default.
    let candidates = Action::Quit
        .fallbacks()
        .iter()
        .map(|t| t.parse::<Chord>().expect("the quit chords are valid"))
        .filter(|c| !tried.contains(c) && !others.contains(c));
    let id = Action::Quit.hotkey_id();
    for c in candidates {
        match reg.register(id, &c) {
            Ok(()) => {
                if let Some(o) = state.outcomes.iter_mut().find(|o| o.action == Action::Quit) {
                    o.bound = Some(c);
                }
                return Some(c);
            }
            Err(code) => {
                if let Some(o) = state.outcomes.iter_mut().find(|o| o.action == Action::Quit) {
                    o.failures.push((c, code));
                }
            }
        }
    }
    None
}

/// Every action back to its default chord (with the documented fallbacks when a default is taken by another
/// program), registered at once. Quit goes first and keeps its chord when its default cannot be registered (it is
/// never left unbound). Returns the registration problems (empty when every default went through).
pub fn reset_all(reg: &mut dyn Registrar, state: &mut Registration) -> Vec<String> {
    for o in state
        .outcomes
        .iter()
        .filter(|o| o.action != Action::Quit && o.bound.is_some())
    {
        reg.unregister(o.action.hotkey_id());
    }
    if state.menu_fallback.take().is_some() {
        reg.unregister(MENU_FALLBACK_ID);
    }
    state.outcomes.retain(|o| o.action == Action::Quit);
    // Quit to its default now that no other action holds a chord (the old one is kept when the default fails).
    let quit = Action::Quit.default_chord();
    if state
        .outcome(Action::Quit)
        .is_none_or(|o| o.bound != Some(quit) || o.wanted != quit)
    {
        let _ = set_chord(reg, state, Action::Quit, quit);
    }
    let plan: Vec<Attempt> = plan_registration(&Bindings::default())
        .into_iter()
        .filter(|a| a.action != Action::Quit)
        .collect();
    let fresh = register_plan(reg, &plan, state.bound(Action::Quit).into_iter().collect());
    state.outcomes.extend(fresh.outcomes);
    state.outcomes.sort_by_key(|o| o.action);
    update_menu_fallback(reg, state);
    state.problems()
}

/// Which character the installed keyboard layouts type for a key with AltGr held (`vk`, `shift`): a description
/// such as `'@' (German keyboard layout)`, `None` when no layout types one. The live app uses
/// `keyboard_layout::altgr_text` (Win32 `ToUnicodeEx`); the tests a fake.
pub type LayoutProbe = fn(vk: u16, shift: bool) -> Option<String>;

/// The menu's warning for a chord that also fires on AltGr (review D-01): Ctrl+Alt (without Win) on a letter, a
/// digit or Space that types a character with AltGr on an installed layout. `None` for any other chord, or when `probe` finds no
/// character.
pub fn altgr_conflict(chord: Chord, probe: LayoutProbe) -> Option<String> {
    let m = chord.mods;
    // Only the character keys a chord can use (letters, digits, Space) type something; function and navigation
    // keys never do.
    let character_key = matches!(chord.vk, 0x20 | 0x30..=0x39 | 0x41..=0x5A);
    if !(m.contains(Mods::CTRL) && m.contains(Mods::ALT)) || m.contains(Mods::WIN) || !character_key {
        return None;
    }
    let what = probe(chord.vk, m.contains(Mods::SHIFT))?;
    let altgr = if m.contains(Mods::SHIFT) {
        format!("AltGr+Shift+{}", key_name(chord.vk))
    } else {
        format!("AltGr+{}", key_name(chord.vk))
    };
    Some(format!(
        "{chord} is also {altgr}, which types {what}: while Phoxium runs, that character cannot be typed in any \
         program, and typing it triggers this hotkey. A function key or a navigation key avoids this."
    ))
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

/// Runs the plan: for each action, the candidates in order until one registers. A candidate already registered
/// for another action is skipped without asking the system (it would fail with error 1409 anyway). The menu's
/// safety chord is not set here: the output registers it after ([`update_menu_fallback`]).
pub fn register_all(reg: &mut dyn Registrar, plan: &[Attempt]) -> Registration {
    register_plan(reg, plan, Vec::new())
}

/// [`register_all`] with chords already registered elsewhere in this process (`taken`).
fn register_plan(reg: &mut dyn Registrar, plan: &[Attempt], mut taken: Vec<Chord>) -> Registration {
    let mut outcomes = Vec::new();
    for attempt in plan {
        let mut failures = Vec::new();
        let mut bound = None;
        for c in &attempt.candidates {
            let result = if taken.contains(c) {
                Err(ERROR_HOTKEY_ALREADY_REGISTERED)
            } else {
                reg.register(attempt.action.hotkey_id(), c)
            };
            match result {
                Ok(()) => {
                    taken.push(*c);
                    bound = Some(*c);
                    break;
                }
                Err(code) => failures.push((*c, code)),
            }
        }
        outcomes.push(Outcome {
            action: attempt.action,
            wanted: attempt.candidates[0],
            bound,
            failures,
        });
    }
    Registration {
        outcomes,
        menu_fallback: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeSet, HashMap};

    fn chord(text: &str) -> Chord {
        text.parse().unwrap_or_else(|e| panic!("{text}: {e}"))
    }

    #[test]
    fn parses_and_formats_the_documented_examples() {
        assert_eq!(chord("F10").to_string(), "F10");
        assert_eq!(chord("Ctrl+Alt+F12").to_string(), "Ctrl+Alt+F12");
        assert_eq!(chord("Ctrl+Alt+PageDown").to_string(), "Ctrl+Alt+PageDown");
        assert_eq!(
            chord("Ctrl+Alt+PageDown"),
            Chord {
                mods: Mods::CTRL | Mods::ALT,
                vk: 0x22
            }
        );
        assert_eq!(
            chord("F10"),
            Chord {
                mods: Mods::NONE,
                vk: 0x79
            }
        );
        assert_eq!(chord("Ctrl+Alt+Q").vk, b'Q' as u16);
    }

    #[test]
    fn parsing_is_case_insensitive_and_normalises() {
        assert_eq!(chord("ctrl+alt+pagedown"), chord("Ctrl+Alt+PageDown"));
        assert_eq!(chord("CTRL + alt + PGDN").to_string(), "Ctrl+Alt+PageDown");
        assert_eq!(chord("f11").to_string(), "F11");
        assert_eq!(
            chord("alt+ctrl+q").to_string(),
            "Ctrl+Alt+Q",
            "modifier order is canonical"
        );
        assert_eq!(chord("Control+Shift+del").to_string(), "Ctrl+Shift+Delete");
        assert_eq!(chord("Win+Ctrl+7").to_string(), "Ctrl+Win+7");
    }

    #[test]
    fn every_key_name_round_trips() {
        let mut seen = BTreeSet::new();
        for vk in (0x70..=0x87u16)
            .chain(0x30..=0x39)
            .chain(0x41..=0x5A)
            .chain(NAMED_KEYS.iter().map(|k| k.1))
        {
            let c = Chord { mods: Mods::CTRL, vk };
            let back: Chord = c.to_string().parse().unwrap();
            assert_eq!(back, c, "{c}");
            assert!(seen.insert(c.to_string()), "{c} appears twice");
        }
    }

    #[test]
    fn rejects_unknown_keys_and_malformed_chords() {
        assert_eq!("".parse::<Chord>(), Err(ChordError::Empty));
        assert_eq!("  ".parse::<Chord>(), Err(ChordError::Empty));
        assert_eq!("Ctrl++Q".parse::<Chord>(), Err(ChordError::EmptyPart));
        assert_eq!("Ctrl+".parse::<Chord>(), Err(ChordError::EmptyPart));
        assert_eq!(
            "Ctrl+Alt+Banana".parse::<Chord>(),
            Err(ChordError::UnknownKey("Banana".into()))
        );
        assert_eq!("F25".parse::<Chord>(), Err(ChordError::UnknownKey("F25".into())));
        assert_eq!("F0".parse::<Chord>(), Err(ChordError::UnknownKey("F0".into())));
        assert_eq!("Ctrl+Alt".parse::<Chord>(), Err(ChordError::MissingKey));
        assert_eq!("Ctrl+F5+A".parse::<Chord>(), Err(ChordError::NotAModifier("F5".into())));
        assert_eq!(
            "Ctrl+Ctrl+A".parse::<Chord>(),
            Err(ChordError::DuplicateModifier("Ctrl".into()))
        );
        assert_eq!("Esc".parse::<Chord>(), Err(ChordError::UnknownKey("Esc".into())));
        assert_eq!("QQ".parse::<Chord>(), Err(ChordError::UnknownKey("QQ".into())));
    }

    #[test]
    fn a_bare_ordinary_key_is_refused_but_function_keys_may_go_bare() {
        assert_eq!("Q".parse::<Chord>(), Err(ChordError::NeedsModifier("Q".into())));
        assert_eq!("Shift+Q".parse::<Chord>(), Err(ChordError::NeedsModifier("Q".into())));
        assert_eq!(
            "PageDown".parse::<Chord>(),
            Err(ChordError::NeedsModifier("PageDown".into()))
        );
        assert!("F1".parse::<Chord>().is_ok());
        assert!("Shift+F24".parse::<Chord>().is_ok());
        assert!("Alt+Q".parse::<Chord>().is_ok());
    }

    #[test]
    fn error_messages_are_readable() {
        let m = "Ctrl+Alt+Banana".parse::<Chord>().unwrap_err().to_string();
        assert!(m.contains("Banana") && m.contains("unknown key"), "{m}");
        let m = "Q".parse::<Chord>().unwrap_err().to_string();
        assert!(m.contains("bare key Q"), "{m}");
    }

    #[test]
    fn defaults_are_valid_distinct_and_match_the_settings_table() {
        let mut seen = BTreeSet::new();
        for a in Action::ALL {
            assert!(seen.insert(a.default_chord()), "{a:?} shares its default");
            let in_settings = crate::settings::DEFAULT_HOTKEYS
                .iter()
                .find(|(id, _)| *id == a.id())
                .unwrap_or_else(|| panic!("{} missing from settings::DEFAULT_HOTKEYS", a.id()));
            assert_eq!(in_settings.1, a.default_text(), "{}", a.id());
            assert_eq!(
                a.default_chord().to_string(),
                a.default_text(),
                "default text is canonical"
            );
        }
        assert_eq!(crate::settings::DEFAULT_HOTKEYS.len(), Action::ALL.len());
        assert_eq!(Action::Quit.default_text(), "Ctrl+Alt+End");
        assert_eq!(Action::ToggleMenu.default_text(), "F10");
        assert_eq!(Action::ToggleFullscreen.default_text(), "F11");
        assert_eq!(Action::ToggleOverlay.default_text(), "Ctrl+Alt+F9");
        assert_eq!(Action::ToggleBypass.default_text(), "Ctrl+Alt+F7");
        assert_eq!(Action::ToggleClickThrough.default_text(), "Ctrl+Alt+F6");
        assert_eq!(
            Action::from_id("toggle_click_through"),
            Some(Action::ToggleClickThrough)
        );
        assert_eq!(
            Action::from_hotkey_id(Action::ToggleClickThrough.hotkey_id()),
            Some(Action::ToggleClickThrough)
        );
        assert_ne!(Action::ToggleClickThrough.hotkey_id(), MENU_FALLBACK_ID);
        // No chord (default or fallback) belongs to two actions, the menu's own fallbacks aside.
        let mut owner: BTreeMap<Chord, Action> = BTreeMap::new();
        for a in Action::ALL {
            for text in std::iter::once(a.default_text()).chain(a.fallbacks().iter().copied()) {
                if let Some(b) = owner.insert(chord(text), a) {
                    assert_eq!(b, a, "{text} is a chord of {b:?} and {a:?}");
                }
            }
        }
    }

    /// Review D-01: no default or fallback chord can fire on AltGr+<character> (Ctrl+Alt on a letter, a digit or
    /// Space), none uses the arrows (screen rotation of some graphics drivers) or the Intel graphics hotkeys
    /// (Ctrl+Alt+F1, F3, F4, F8), and the old defaults are not taken by any default.
    #[test]
    fn defaults_and_fallbacks_avoid_altgr_and_driver_chords() {
        let character_key = |vk: u16| matches!(vk, 0x20 | 0x30..=0x39 | 0x41..=0x5A);
        let intel = ["Ctrl+Alt+F1", "Ctrl+Alt+F3", "Ctrl+Alt+F4", "Ctrl+Alt+F8"].map(chord);
        for a in Action::ALL {
            for text in std::iter::once(a.default_text()).chain(a.fallbacks().iter().copied()) {
                let c = chord(text);
                let ctrl_alt = c.mods.contains(Mods::CTRL | Mods::ALT);
                assert!(!(ctrl_alt && character_key(c.vk)), "{a:?}: {c} fires on AltGr");
                assert!(!(0x25..=0x28).contains(&c.vk), "{a:?}: {c} uses an arrow");
                assert!(!intel.contains(&c), "{a:?}: {c} is an Intel graphics hotkey");
                // A fake layout where every key types a character: no warning for any of them.
                assert_eq!(altgr_conflict(c, |_, _| Some("'x'".into())), None, "{c}");
            }
            if let Some(old) = a.old_default_text() {
                assert_ne!(chord(old), a.default_chord());
                assert!(Action::ALL.iter().all(|b| b.default_chord() != chord(old)), "{old}");
            }
        }
        assert_eq!(Action::Quit.old_default_text(), Some("Ctrl+Alt+Q"));
        assert_eq!(Action::ToggleMenu.old_default_text(), None);
    }

    /// Review D-01: a chord with Ctrl+Alt (no Win) on a key that types a character under AltGr is reported, with
    /// the AltGr spelling and what it types; others are not.
    #[test]
    fn a_ctrl_alt_character_chord_is_reported_when_a_layout_types_something() {
        fn german(vk: u16, shift: bool) -> Option<String> {
            (vk == u16::from(b'Q') && !shift).then(|| "'@' (German keyboard layout)".to_string())
        }
        let w = altgr_conflict(chord("Ctrl+Alt+Q"), german).expect("AltGr+Q is '@'");
        assert!(
            w.contains("Ctrl+Alt+Q is also AltGr+Q") && w.contains("'@' (German"),
            "{w}"
        );
        assert_eq!(
            altgr_conflict(chord("Ctrl+Alt+Shift+Q"), german),
            None,
            "AltGr+Shift+Q types nothing"
        );
        assert_eq!(altgr_conflict(chord("Ctrl+Alt+W"), german), None);
        assert_eq!(
            altgr_conflict(chord("Ctrl+Q"), german),
            None,
            "no Alt: AltGr cannot fire it"
        );
        assert_eq!(altgr_conflict(chord("Alt+Q"), german), None);
        assert_eq!(
            altgr_conflict(chord("Ctrl+Alt+Win+Q"), german),
            None,
            "Win must be held as well"
        );
        let w = altgr_conflict(chord("Ctrl+Alt+Shift+7"), |_, shift| shift.then(|| "'x'".to_string())).unwrap();
        assert!(w.contains("AltGr+Shift+7"), "{w}");
    }

    #[test]
    fn ids_and_hotkey_ids_round_trip() {
        for a in Action::ALL {
            assert_eq!(Action::from_id(a.id()), Some(a));
            assert_eq!(Action::from_hotkey_id(a.hotkey_id()), Some(a));
        }
        assert_eq!(Action::from_id("nope"), None);
        assert_eq!(Action::from_hotkey_id(0), None);
        assert_eq!(Action::from_hotkey_id(99), None);
        assert_eq!(Action::Quit.hotkey_id(), 1, "the quit id stays 1, as before T-54");
    }

    fn map(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries.iter().map(|(a, c)| (a.to_string(), c.to_string())).collect()
    }

    #[test]
    fn settings_defaults_give_the_default_bindings_without_warnings() {
        let (b, w) = Bindings::from_settings(&crate::settings::default_hotkeys());
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(b, Bindings::default());
        let (b, w) = Bindings::from_settings(&BTreeMap::new());
        assert!(w.is_empty());
        assert_eq!(b, Bindings::default());
    }

    #[test]
    fn a_valid_override_is_used() {
        let (b, w) = Bindings::from_settings(&map(&[("toggle_menu", "Ctrl+Alt+F12"), ("toggle_overlay", "F9")]));
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(b.get(Action::ToggleMenu), Some(chord("Ctrl+Alt+F12")));
        assert_eq!(b.get(Action::ToggleOverlay), Some(chord("F9")));
        assert_eq!(b.get(Action::ToggleBypass), Some(Action::ToggleBypass.default_chord()));
    }

    #[test]
    fn an_invalid_entry_falls_back_to_the_default_with_a_warning() {
        let (b, w) = Bindings::from_settings(&map(&[("toggle_menu", "Ctrl+Alt+Banana"), ("next_preset", "Q")]));
        assert_eq!(b.get(Action::ToggleMenu), Some(chord("F10")));
        assert_eq!(b.get(Action::NextPreset), Some(chord("Ctrl+Alt+PageDown")));
        assert_eq!(w.len(), 2, "{w:?}");
        assert!(
            w[0].contains("toggle_menu") && w[0].contains("Banana") && w[0].contains("F10"),
            "{w:?}"
        );
        assert!(w[1].contains("next_preset") && w[1].contains("bare key"), "{w:?}");
    }

    #[test]
    fn quit_can_be_rebound_and_always_gets_a_chord() {
        let (b, w) = Bindings::from_settings(&map(&[("quit", "Ctrl+Shift+F12")]));
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(b.get(Action::Quit), Some(chord("Ctrl+Shift+F12")));
        // Writing the default is not a warning, in any case.
        let (b, w) = Bindings::from_settings(&map(&[("quit", "ctrl+alt+end")]));
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(b.get(Action::Quit), Some(Action::Quit.default_chord()));
        // An invalid chord gives the default; another action asking for the quit chord loses (quit comes first).
        let (b, w) = Bindings::from_settings(&map(&[("quit", "Q"), ("toggle_menu", "Ctrl+Alt+End")]));
        assert_eq!(b.get(Action::Quit), Some(chord("Ctrl+Alt+End")));
        assert_eq!(b.get(Action::ToggleMenu), Some(chord("F10")));
        assert_eq!(w.len(), 2, "{w:?}");
    }

    #[test]
    fn colliding_chords_are_rejected_with_a_message() {
        // The overlay asks for the menu's F10: rejected, its default is used.
        let (b, w) = Bindings::from_settings(&map(&[("toggle_overlay", "F10")]));
        assert_eq!(b.get(Action::ToggleMenu), Some(chord("F10")));
        assert_eq!(b.get(Action::ToggleOverlay), Some(chord("Ctrl+Alt+F9")));
        assert_eq!(w.len(), 1);
        assert!(
            w[0].contains("toggle_overlay") && w[0].contains("already used by toggle_menu"),
            "{w:?}"
        );
        // Nothing can take the quit chord.
        let (b, w) = Bindings::from_settings(&map(&[("toggle_bypass", "Ctrl+Alt+End")]));
        assert_eq!(b.get(Action::ToggleBypass), Some(chord("Ctrl+Alt+F7")));
        assert!(w[0].contains("already used by quit"), "{w:?}");
        // Same chord for two rebinds: the earlier action keeps it.
        let (b, w) = Bindings::from_settings(&map(&[("next_preset", "F8"), ("previous_preset", "F8")]));
        assert_eq!(b.get(Action::NextPreset), Some(chord("F8")));
        assert_eq!(b.get(Action::PreviousPreset), Some(chord("Ctrl+Alt+PageUp")));
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn an_action_stays_unbound_when_chord_and_default_are_both_taken() {
        // The menu takes the bypass default, and the bypass asks for the menu's chord.
        let (b, w) = Bindings::from_settings(&map(&[
            ("toggle_menu", "Ctrl+Alt+F7"),
            ("toggle_bypass", "Ctrl+Alt+F7"),
        ]));
        assert_eq!(b.get(Action::ToggleMenu), Some(chord("Ctrl+Alt+F7")));
        assert_eq!(b.get(Action::ToggleBypass), None);
        assert!(w.iter().any(|m| m.contains("left unbound")), "{w:?}");
        // Unbound actions have no registration attempt.
        assert!(plan_registration(&b).iter().all(|a| a.action != Action::ToggleBypass));
    }

    #[test]
    fn unknown_action_ids_are_ignored_with_a_warning() {
        let (b, w) = Bindings::from_settings(&map(&[("screenshot", "F9")]));
        assert_eq!(b, Bindings::default());
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("screenshot"));
    }

    #[test]
    fn bindings_write_back_as_settings_text() {
        assert_eq!(Bindings::default().to_settings(), crate::settings::default_hotkeys());
    }

    // ---- registration with a fake registrar ----

    /// Chords other programs own, by Win32 error; everything else registers. Records the calls.
    #[derive(Default)]
    struct Fake {
        owned_elsewhere: HashMap<Chord, u32>,
        registered: HashMap<i32, Chord>,
        calls: Vec<(i32, Chord)>,
        unregistered: Vec<i32>,
    }

    impl Fake {
        fn taking(chords: &[&str]) -> Fake {
            Fake {
                owned_elsewhere: chords
                    .iter()
                    .map(|c| (chord(c), ERROR_HOTKEY_ALREADY_REGISTERED))
                    .collect(),
                ..Fake::default()
            }
        }
    }

    impl Registrar for Fake {
        fn register(&mut self, id: i32, c: &Chord) -> Result<(), u32> {
            self.calls.push((id, *c));
            if let Some(e) = self.owned_elsewhere.get(c) {
                return Err(*e);
            }
            // The system refuses a chord twice, also within this process.
            if self.registered.values().any(|r| r == c) {
                return Err(ERROR_HOTKEY_ALREADY_REGISTERED);
            }
            self.registered.insert(id, *c);
            Ok(())
        }

        fn unregister(&mut self, id: i32) {
            self.registered.remove(&id);
            self.unregistered.push(id);
        }
    }

    /// Re-registration failure paths: the click-through chord and its fallback both owned by another program leave
    /// that one action unbound with a problem naming it (everything else works, the menu row still switches it);
    /// a menu rebind whose new chord is refused and whose old chord cannot be taken back keeps the menu reachable
    /// through the safety chord.
    #[test]
    fn registration_failures_never_cost_the_menu() {
        let mut fake = Fake::taking(&["Ctrl+Alt+F6", "Ctrl+Alt+Shift+F6"]);
        let mut reg = register_all(&mut fake, &plan_registration(&Bindings::default()));
        assert_eq!(reg.bound(Action::ToggleClickThrough), None);
        let p = reg.problems().join(" ");
        assert!(p.contains("toggle_click_through") && p.contains("1409"), "{p}");
        for a in Action::ALL.into_iter().filter(|a| *a != Action::ToggleClickThrough) {
            assert_eq!(reg.bound(a), Some(a.default_chord()), "{a:?}");
        }
        // The menu: the new chord is refused, and F10 is taken by another program the moment it is released.
        fake.owned_elsewhere
            .insert(chord("Ctrl+Alt+M"), ERROR_HOTKEY_ALREADY_REGISTERED);
        fake.owned_elsewhere
            .insert(chord("F10"), ERROR_HOTKEY_ALREADY_REGISTERED);
        let e = rebind(&mut fake, &mut reg, Action::ToggleMenu, chord("Ctrl+Alt+M")).unwrap_err();
        assert!(e.contains("could not be taken back"), "{e}");
        assert_eq!(reg.bound(Action::ToggleMenu), None);
        assert_eq!(
            reg.menu_fallback,
            Some(chord("Ctrl+Alt+F10")),
            "the safety chord opens the menu"
        );
        assert_eq!(fake.registered[&MENU_FALLBACK_ID], chord("Ctrl+Alt+F10"));
        assert_eq!(Action::from_hotkey_id(MENU_FALLBACK_ID), Some(Action::ToggleMenu));
        // Reset all: the menu back on a documented chord (F10 is still owned: its first fallback), click-through
        // still reported, Quit kept.
        let problems = reset_all(&mut fake, &mut reg);
        assert!(reg.bound(Action::ToggleMenu).is_some() || reg.menu_fallback.is_some());
        assert!(problems.join(" ").contains("toggle_click_through"), "{problems:?}");
        assert_eq!(reg.bound(Action::Quit), Some(Action::Quit.default_chord()));
    }

    #[test]
    fn the_plan_lists_the_wanted_chord_first_then_the_fallbacks() {
        let plan = plan_registration(&Bindings::default());
        assert_eq!(plan.len(), Action::ALL.len());
        assert_eq!(plan[0].action, Action::Quit, "quit is registered first");
        assert_eq!(
            plan[0].candidates,
            vec![chord("Ctrl+Alt+End"), chord("Ctrl+Alt+Shift+End")],
            "quit has one fallback"
        );
        let menu = plan.iter().find(|a| a.action == Action::ToggleMenu).unwrap();
        // F10 is the wanted chord, so the plain F10 fallback is not repeated; the list ends with Ctrl+Alt+F12.
        assert_eq!(
            menu.candidates,
            vec![chord("F10"), chord("Ctrl+Alt+F10"), chord("Ctrl+Alt+F12")]
        );
        for a in &plan {
            let set: BTreeSet<_> = a.candidates.iter().collect();
            assert_eq!(set.len(), a.candidates.len(), "{:?} lists a chord twice", a.action);
        }
    }

    #[test]
    fn a_fallback_never_steals_another_actions_wanted_chord() {
        // Fullscreen is rebound to Ctrl+Alt+F10, the menu's first fallback.
        let (b, _) = Bindings::from_settings(&map(&[("toggle_fullscreen", "Ctrl+Alt+F10")]));
        let plan = plan_registration(&b);
        let menu = plan.iter().find(|a| a.action == Action::ToggleMenu).unwrap();
        assert_eq!(menu.candidates, vec![chord("F10"), chord("Ctrl+Alt+F12")]);
    }

    #[test]
    fn all_chords_free_registers_the_wanted_ones_and_reports_nothing() {
        let mut fake = Fake::default();
        let reg = register_all(&mut fake, &plan_registration(&Bindings::default()));
        assert!(!reg.has_problems());
        assert!(reg.problems().is_empty());
        for a in Action::ALL {
            assert_eq!(reg.bound(a), Some(a.default_chord()), "{a:?}");
            assert_eq!(fake.registered[&a.hotkey_id()], a.default_chord());
        }
        assert_eq!(
            fake.calls.len(),
            Action::ALL.len(),
            "one call per action when nothing fails"
        );
        assert!(reg.summary().contains("settings menu F10"), "{}", reg.summary());
    }

    #[test]
    fn f10_taken_falls_back_and_names_the_chord_and_the_error() {
        let mut fake = Fake::taking(&["F10"]);
        let reg = register_all(&mut fake, &plan_registration(&Bindings::default()));
        assert_eq!(reg.bound(Action::ToggleMenu), Some(chord("Ctrl+Alt+F10")));
        assert!(reg.has_problems());
        let problems = reg.problems();
        assert_eq!(problems.len(), 1, "{problems:?}");
        let p = &problems[0];
        assert!(
            p.contains("toggle_menu") && p.contains("F10") && p.contains("1409"),
            "{p}"
        );
        assert!(p.contains("using Ctrl+Alt+F10 instead"), "{p}");
        // The other actions are unaffected.
        assert_eq!(reg.bound(Action::ToggleFullscreen), Some(chord("F11")));
        assert_eq!(reg.bound(Action::Quit), Some(chord("Ctrl+Alt+End")));
    }

    #[test]
    fn f12_taken_while_f10_is_wanted_changes_nothing() {
        // The reference PC case: F12 is owned by another program (error 1409), the default menu key is F10.
        let mut fake = Fake::taking(&["F12"]);
        let reg = register_all(&mut fake, &plan_registration(&Bindings::default()));
        assert!(!reg.has_problems());
        assert_eq!(reg.bound(Action::ToggleMenu), Some(chord("F10")));
        // A user who still asks for F12 gets the fallback list, starting with the plain F10, and a message.
        let (b, w) = Bindings::from_settings(&map(&[("toggle_menu", "F12")]));
        assert!(w.is_empty());
        let mut fake = Fake::taking(&["F12"]);
        let reg = register_all(&mut fake, &plan_registration(&b));
        assert_eq!(reg.bound(Action::ToggleMenu), Some(chord("F10")));
        let p = reg.problems();
        assert_eq!(p.len(), 1);
        assert!(
            p[0].contains("F12") && p[0].contains("1409") && p[0].contains("using F10 instead"),
            "{p:?}"
        );
    }

    #[test]
    fn every_candidate_failing_leaves_the_action_unavailable_with_all_errors_listed() {
        let mut fake = Fake::taking(&["F10", "Ctrl+Alt+F10", "Ctrl+Alt+F12"]);
        let reg = register_all(&mut fake, &plan_registration(&Bindings::default()));
        assert_eq!(reg.bound(Action::ToggleMenu), None);
        let o = reg.outcomes.iter().find(|o| o.action == Action::ToggleMenu).unwrap();
        assert_eq!(o.failures.len(), 3);
        let m = o.message().unwrap();
        assert!(
            m.contains("F10") && m.contains("Ctrl+Alt+F12") && m.contains("unavailable"),
            "{m}"
        );
        assert!(reg.summary().contains("settings menu unavailable"), "{}", reg.summary());
    }

    #[test]
    fn a_double_registration_of_one_chord_reports_the_failing_chord() {
        // Both actions want F8 (a hand-built plan, as a settings file cannot give it after validation).
        let plan = vec![
            Attempt {
                action: Action::ToggleOverlay,
                candidates: vec![chord("F8")],
            },
            Attempt {
                action: Action::ToggleBypass,
                candidates: vec![chord("F8")],
            },
        ];
        let mut fake = Fake::default();
        let reg = register_all(&mut fake, &plan);
        assert_eq!(reg.bound(Action::ToggleOverlay), Some(chord("F8")));
        assert_eq!(reg.bound(Action::ToggleBypass), None);
        let p = reg.problems();
        assert_eq!(p.len(), 1);
        assert!(
            p[0].contains("toggle_bypass") && p[0].contains("F8") && p[0].contains("1409"),
            "{p:?}"
        );
        assert_eq!(fake.calls.len(), 1, "the second try is not sent to the system");
    }

    #[test]
    fn quit_failing_is_visible_to_the_caller() {
        let mut fake = Fake::taking(&["Ctrl+Alt+End"]);
        let reg = register_all(&mut fake, &plan_registration(&Bindings::default()));
        assert_eq!(
            reg.bound(Action::Quit),
            Some(chord("Ctrl+Alt+Shift+End")),
            "the fallback"
        );
        assert!(reg.problems()[0].contains("Ctrl+Alt+End"));
        let mut fake = Fake::taking(&["Ctrl+Alt+End", "Ctrl+Alt+Shift+End"]);
        let reg = register_all(&mut fake, &plan_registration(&Bindings::default()));
        assert_eq!(reg.bound(Action::Quit), None, "output.rs refuses to go fullscreen then");
        assert!(reg.problems()[0].contains("Ctrl+Alt+Shift+End"));
    }

    #[test]
    fn unregister_all_releases_exactly_the_registered_ids() {
        let mut fake = Fake::taking(&["Ctrl+Alt+F9"]);
        let reg = register_all(&mut fake, &plan_registration(&Bindings::default()));
        fake.unregistered.clear();
        reg.unregister_all(&mut fake);
        let mut ids = fake.unregistered.clone();
        ids.sort();
        let mut want: Vec<i32> = Action::ALL.iter().map(|a| a.hotkey_id()).collect();
        want.sort();
        assert_eq!(
            ids, want,
            "the overlay fell back to its fallback chord and is registered too"
        );
        assert!(fake.registered.is_empty());
    }

    // ---- rebinding from the menu ----

    fn started(fake: &mut Fake) -> Registration {
        let mut reg = register_all(fake, &plan_registration(&Bindings::default()));
        update_menu_fallback(fake, &mut reg);
        reg
    }

    #[test]
    fn a_key_press_while_waiting_becomes_a_chord_a_cancel_or_a_reset() {
        let ctrl_alt = Mods::CTRL | Mods::ALT;
        assert_eq!(capture_key(0x11, Mods::CTRL), Captured::Waiting, "Ctrl alone");
        assert_eq!(capture_key(0xA4, Mods::ALT), Captured::Waiting, "left Alt alone");
        assert_eq!(capture_key(0x5B, Mods::WIN), Captured::Waiting);
        assert_eq!(capture_key(0x1B, Mods::NONE), Captured::Cancel);
        assert_eq!(capture_key(0x1B, Mods::CTRL), Captured::Cancel);
        assert_eq!(capture_key(0x08, Mods::NONE), Captured::Default);
        assert_eq!(capture_key(0x2E, Mods::NONE), Captured::Default);
        assert_eq!(
            capture_key(0x2E, ctrl_alt),
            Captured::Chord(chord("Ctrl+Alt+Delete")),
            "Delete with modifiers is a chord"
        );
        assert_eq!(capture_key(b'M' as u16, ctrl_alt), Captured::Chord(chord("Ctrl+Alt+M")));
        assert_eq!(capture_key(0x78, Mods::NONE), Captured::Chord(chord("F9")));
        assert_eq!(capture_key(0x78, Mods::SHIFT), Captured::Chord(chord("Shift+F9")));
        let Captured::Invalid(m) = capture_key(b'Q' as u16, Mods::SHIFT) else {
            panic!("a bare letter")
        };
        assert!(m.contains("Q needs Ctrl, Alt or Win"), "{m}");
        let Captured::Invalid(m) = capture_key(0xBA, Mods::CTRL) else {
            panic!("an OEM key")
        };
        assert!(m.contains("cannot be a hotkey"), "{m}");
    }

    #[test]
    fn a_rebind_registers_the_new_chord_at_once_and_writes_it_back() {
        let mut fake = Fake::default();
        let mut reg = started(&mut fake);
        assert_eq!(reg.menu_fallback, None, "the default menu key needs no safety chord");
        let msg = rebind(&mut fake, &mut reg, Action::ToggleOverlay, chord("Ctrl+Alt+F5")).unwrap();
        assert!(msg.contains("Overlay: Ctrl+Alt+F5"), "{msg}");
        assert_eq!(reg.bound(Action::ToggleOverlay), Some(chord("Ctrl+Alt+F5")));
        assert_eq!(
            fake.registered[&Action::ToggleOverlay.hotkey_id()],
            chord("Ctrl+Alt+F5")
        );
        assert!(
            !fake.registered.values().any(|c| *c == chord("Ctrl+Alt+F9")),
            "the old chord is released"
        );
        let text = reg.to_settings();
        assert_eq!(text["toggle_overlay"], "Ctrl+Alt+F5");
        assert_eq!(text["quit"], "Ctrl+Alt+End");
        // The written text reads back as the same bindings, without a warning.
        let (b, w) = Bindings::from_settings(&text);
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(b.get(Action::ToggleOverlay), Some(chord("Ctrl+Alt+F5")));
        // The same chord again: nothing to do.
        let msg = rebind(&mut fake, &mut reg, Action::ToggleOverlay, chord("Ctrl+Alt+F5")).unwrap();
        assert!(msg.contains("unchanged"), "{msg}");
    }

    #[test]
    fn a_chord_another_program_owns_is_refused_and_the_old_one_kept() {
        let mut fake = Fake::default();
        let mut reg = started(&mut fake);
        fake.owned_elsewhere
            .insert(chord("Ctrl+Alt+F5"), ERROR_HOTKEY_ALREADY_REGISTERED);
        let e = rebind(&mut fake, &mut reg, Action::ToggleOverlay, chord("Ctrl+Alt+F5")).unwrap_err();
        assert!(
            e.contains("Ctrl+Alt+F5 cannot be used") && e.contains("1409") && e.contains("Ctrl+Alt+F9 is kept"),
            "{e}"
        );
        assert_eq!(reg.bound(Action::ToggleOverlay), Some(chord("Ctrl+Alt+F9")));
        assert_eq!(
            fake.registered[&Action::ToggleOverlay.hotkey_id()],
            chord("Ctrl+Alt+F9"),
            "registered again"
        );
        assert_eq!(
            reg.to_settings()["toggle_overlay"],
            "Ctrl+Alt+F9",
            "the settings keep the old chord"
        );
    }

    #[test]
    fn conflicts_between_our_own_actions_and_quit_are_refused_without_a_system_call() {
        let mut fake = Fake::default();
        let mut reg = started(&mut fake);
        let calls = fake.calls.len();
        let e = rebind(&mut fake, &mut reg, Action::ToggleOverlay, chord("F11")).unwrap_err();
        assert!(e.contains("F11 is already used for fullscreen"), "{e}");
        let e = rebind(&mut fake, &mut reg, Action::ToggleBypass, chord("Ctrl+Alt+End")).unwrap_err();
        assert!(e.contains("already used for quit"), "{e}");
        let e = rebind(&mut fake, &mut reg, Action::Quit, chord("F10")).unwrap_err();
        assert!(e.contains("F10 is already used for settings menu"), "{e}");
        assert_eq!(fake.calls.len(), calls);
        assert_eq!(reg, started(&mut Fake::default()));
    }

    /// Review D-01: Quit is rebindable like the others, keeps its chord when the new one is refused, and falls
    /// back to its default (then its fallback) when even the old chord cannot be taken back.
    #[test]
    fn quit_rebinds_keeps_the_old_chord_when_refused_and_is_never_left_unbound() {
        let mut fake = Fake::default();
        let mut reg = started(&mut fake);
        let msg = rebind(&mut fake, &mut reg, Action::Quit, chord("Ctrl+Shift+F12")).unwrap();
        assert!(msg.contains("Quit: Ctrl+Shift+F12"), "{msg}");
        assert_eq!(fake.registered[&Action::Quit.hotkey_id()], chord("Ctrl+Shift+F12"));
        assert_eq!(reg.to_settings()["quit"], "Ctrl+Shift+F12");
        // Refused by the system: the old chord stays.
        fake.owned_elsewhere
            .insert(chord("Ctrl+Alt+Home"), ERROR_HOTKEY_ALREADY_REGISTERED);
        let e = rebind(&mut fake, &mut reg, Action::Quit, chord("Ctrl+Alt+Home")).unwrap_err();
        assert!(e.contains("Ctrl+Shift+F12 is kept"), "{e}");
        assert_eq!(reg.bound(Action::Quit), Some(chord("Ctrl+Shift+F12")));
        // The old chord is taken by another program in between: Quit goes to its default.
        fake.owned_elsewhere
            .insert(chord("Ctrl+Shift+F12"), ERROR_HOTKEY_ALREADY_REGISTERED);
        let e = rebind(&mut fake, &mut reg, Action::Quit, chord("Ctrl+Alt+Home")).unwrap_err();
        assert!(e.contains("Quit is on Ctrl+Alt+End now"), "{e}");
        assert_eq!(reg.bound(Action::Quit), Some(chord("Ctrl+Alt+End")));
        assert_eq!(fake.registered[&Action::Quit.hotkey_id()], chord("Ctrl+Alt+End"));
    }

    #[test]
    fn changing_the_menu_key_keeps_the_documented_fallback_registered() {
        let mut fake = Fake::default();
        let mut reg = started(&mut fake);
        rebind(&mut fake, &mut reg, Action::ToggleMenu, chord("Ctrl+Alt+M")).unwrap();
        assert_eq!(reg.bound(Action::ToggleMenu), Some(chord("Ctrl+Alt+M")));
        assert_eq!(reg.menu_fallback, Some(chord("F10")), "F10 still opens the menu");
        assert_eq!(fake.registered[&MENU_FALLBACK_ID], chord("F10"));
        assert_eq!(Action::from_hotkey_id(MENU_FALLBACK_ID), Some(Action::ToggleMenu));
        assert!(reg.fallback_note().unwrap().contains("F10 also opens the menu"));
        // Another action takes F10: the safety chord moves to the next documented fallback.
        rebind(&mut fake, &mut reg, Action::ToggleOverlay, chord("F10")).unwrap();
        assert_eq!(reg.bound(Action::ToggleOverlay), Some(chord("F10")));
        assert_eq!(reg.menu_fallback, Some(chord("Ctrl+Alt+F10")));
        assert_eq!(fake.registered[&MENU_FALLBACK_ID], chord("Ctrl+Alt+F10"));
        // F10 back for the menu (the overlay first gives it up): no safety chord any more.
        rebind(&mut fake, &mut reg, Action::ToggleOverlay, chord("Ctrl+Alt+F9")).unwrap();
        rebind(&mut fake, &mut reg, Action::ToggleMenu, chord("F10")).unwrap();
        assert_eq!(reg.menu_fallback, None);
        assert!(!fake.registered.contains_key(&MENU_FALLBACK_ID));
        // A menu key read from the settings at start gets its safety chord too; unregister_all releases it.
        let (b, _) = Bindings::from_settings(&map(&[("toggle_menu", "Ctrl+Alt+F12")]));
        let mut fake = Fake::default();
        let mut reg = register_all(&mut fake, &plan_registration(&b));
        update_menu_fallback(&mut fake, &mut reg);
        assert_eq!(reg.menu_fallback, Some(chord("F10")));
        reg.unregister_all(&mut fake);
        assert!(fake.registered.is_empty());
    }

    #[test]
    fn the_safety_chord_skips_fallbacks_owned_by_other_programs() {
        let mut fake = Fake::taking(&["F10"]);
        let reg = started(&mut fake);
        // The reference PC case with F10 taken: the menu is on Ctrl+Alt+F10, the safety on Ctrl+Alt+F12.
        assert_eq!(reg.bound(Action::ToggleMenu), Some(chord("Ctrl+Alt+F10")));
        assert_eq!(reg.menu_fallback, Some(chord("Ctrl+Alt+F12")));
    }

    #[test]
    fn reset_all_brings_every_default_back_and_drops_the_safety_chord() {
        let mut fake = Fake::default();
        let mut reg = started(&mut fake);
        rebind(&mut fake, &mut reg, Action::ToggleMenu, chord("Ctrl+Alt+M")).unwrap();
        rebind(&mut fake, &mut reg, Action::NextPreset, chord("Ctrl+Alt+N")).unwrap();
        // Quit on the overlay's default and the overlay elsewhere: the reset sorts it out.
        rebind(&mut fake, &mut reg, Action::ToggleOverlay, chord("Ctrl+Alt+F5")).unwrap();
        rebind(&mut fake, &mut reg, Action::Quit, chord("Ctrl+Alt+F9")).unwrap();
        assert!(reg.menu_fallback.is_some());
        let problems = reset_all(&mut fake, &mut reg);
        assert!(problems.is_empty(), "{problems:?}");
        for a in Action::ALL {
            assert_eq!(reg.bound(a), Some(a.default_chord()), "{a:?}");
            assert_eq!(fake.registered[&a.hotkey_id()], a.default_chord(), "{a:?}");
        }
        assert_eq!(reg.menu_fallback, None);
        assert_eq!(reg.to_settings(), crate::settings::default_hotkeys());
        assert_eq!(fake.registered.len(), Action::ALL.len());
    }

    /// The reset never leaves Quit unbound: a default taken by another program keeps the chord in force.
    #[test]
    fn reset_all_keeps_quit_when_its_default_is_taken() {
        let mut fake = Fake::default();
        let mut reg = started(&mut fake);
        rebind(&mut fake, &mut reg, Action::Quit, chord("Ctrl+Shift+F12")).unwrap();
        fake.owned_elsewhere
            .insert(chord("Ctrl+Alt+End"), ERROR_HOTKEY_ALREADY_REGISTERED);
        reset_all(&mut fake, &mut reg);
        assert_eq!(reg.bound(Action::Quit), Some(chord("Ctrl+Shift+F12")));
        assert_eq!(fake.registered[&Action::Quit.hotkey_id()], chord("Ctrl+Shift+F12"));
        assert_eq!(reg.bound(Action::ToggleOverlay), Some(chord("Ctrl+Alt+F9")));
    }

    #[test]
    fn win32_codes_come_out_of_hresults() {
        assert_eq!(win32_code(0x8007_0581u32 as i32), 1409);
        assert_eq!(win32_code(1409), 1409);
        assert!(describe_win32(1409).contains("already registered"));
        assert!(describe_win32(5).contains("Win32 error 5"));
    }
}
