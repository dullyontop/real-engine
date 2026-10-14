//! Binding names: the text form of keys, mouse buttons and gamepad controls.
//!
//! `real.toml` names bindings as `key:KeyW` (a winit [`KeyCode`] variant),
//! `mouse:Left` (a winit [`MouseButton`], or `mouse:Other<n>`) and `pad:South`
//! (a gilrs [`gilrs::Button`]). Sticks are `left` and `right`.
//!
//! winit has no string conversion for `KeyCode`, so the table below lists every
//! variant of winit 0.30.13 (194). A test checks that each name equals the
//! variant's `Debug` text, so a typo cannot slip in.

use winit::event::MouseButton;
use winit::keyboard::KeyCode;

use crate::raw::{Button, Stick};

macro_rules! key_table {
    ($($name:ident),* $(,)?) => {
        /// Every winit key code with its name.
        pub(crate) const KEYS: &[(&str, KeyCode)] = &[$((stringify!($name), KeyCode::$name)),*];
    };
}

key_table![
    Backquote,
    Backslash,
    BracketLeft,
    BracketRight,
    Comma,
    Digit0,
    Digit1,
    Digit2,
    Digit3,
    Digit4,
    Digit5,
    Digit6,
    Digit7,
    Digit8,
    Digit9,
    Equal,
    IntlBackslash,
    IntlRo,
    IntlYen,
    KeyA,
    KeyB,
    KeyC,
    KeyD,
    KeyE,
    KeyF,
    KeyG,
    KeyH,
    KeyI,
    KeyJ,
    KeyK,
    KeyL,
    KeyM,
    KeyN,
    KeyO,
    KeyP,
    KeyQ,
    KeyR,
    KeyS,
    KeyT,
    KeyU,
    KeyV,
    KeyW,
    KeyX,
    KeyY,
    KeyZ,
    Minus,
    Period,
    Quote,
    Semicolon,
    Slash,
    AltLeft,
    AltRight,
    Backspace,
    CapsLock,
    ContextMenu,
    ControlLeft,
    ControlRight,
    Enter,
    SuperLeft,
    SuperRight,
    ShiftLeft,
    ShiftRight,
    Space,
    Tab,
    Convert,
    KanaMode,
    Lang1,
    Lang2,
    Lang3,
    Lang4,
    Lang5,
    NonConvert,
    Delete,
    End,
    Help,
    Home,
    Insert,
    PageDown,
    PageUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    ArrowUp,
    NumLock,
    Numpad0,
    Numpad1,
    Numpad2,
    Numpad3,
    Numpad4,
    Numpad5,
    Numpad6,
    Numpad7,
    Numpad8,
    Numpad9,
    NumpadAdd,
    NumpadBackspace,
    NumpadClear,
    NumpadClearEntry,
    NumpadComma,
    NumpadDecimal,
    NumpadDivide,
    NumpadEnter,
    NumpadEqual,
    NumpadHash,
    NumpadMemoryAdd,
    NumpadMemoryClear,
    NumpadMemoryRecall,
    NumpadMemoryStore,
    NumpadMemorySubtract,
    NumpadMultiply,
    NumpadParenLeft,
    NumpadParenRight,
    NumpadStar,
    NumpadSubtract,
    Escape,
    Fn,
    FnLock,
    PrintScreen,
    ScrollLock,
    Pause,
    BrowserBack,
    BrowserFavorites,
    BrowserForward,
    BrowserHome,
    BrowserRefresh,
    BrowserSearch,
    BrowserStop,
    Eject,
    LaunchApp1,
    LaunchApp2,
    LaunchMail,
    MediaPlayPause,
    MediaSelect,
    MediaStop,
    MediaTrackNext,
    MediaTrackPrevious,
    Power,
    Sleep,
    AudioVolumeDown,
    AudioVolumeMute,
    AudioVolumeUp,
    WakeUp,
    Meta,
    Hyper,
    Turbo,
    Abort,
    Resume,
    Suspend,
    Again,
    Copy,
    Cut,
    Find,
    Open,
    Paste,
    Props,
    Select,
    Undo,
    Hiragana,
    Katakana,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    F13,
    F14,
    F15,
    F16,
    F17,
    F18,
    F19,
    F20,
    F21,
    F22,
    F23,
    F24,
    F25,
    F26,
    F27,
    F28,
    F29,
    F30,
    F31,
    F32,
    F33,
    F34,
    F35,
];

/// Every gilrs gamepad button usable in a binding, with its name.
pub(crate) const PAD_BUTTONS: &[(&str, gilrs::Button)] = &[
    ("South", gilrs::Button::South),
    ("East", gilrs::Button::East),
    ("North", gilrs::Button::North),
    ("West", gilrs::Button::West),
    ("C", gilrs::Button::C),
    ("Z", gilrs::Button::Z),
    ("LeftTrigger", gilrs::Button::LeftTrigger),
    ("LeftTrigger2", gilrs::Button::LeftTrigger2),
    ("RightTrigger", gilrs::Button::RightTrigger),
    ("RightTrigger2", gilrs::Button::RightTrigger2),
    ("Select", gilrs::Button::Select),
    ("Start", gilrs::Button::Start),
    ("Mode", gilrs::Button::Mode),
    ("LeftThumb", gilrs::Button::LeftThumb),
    ("RightThumb", gilrs::Button::RightThumb),
    ("DPadUp", gilrs::Button::DPadUp),
    ("DPadDown", gilrs::Button::DPadDown),
    ("DPadLeft", gilrs::Button::DPadLeft),
    ("DPadRight", gilrs::Button::DPadRight),
];

const MOUSE_BUTTONS: &[(&str, MouseButton)] = &[
    ("Left", MouseButton::Left),
    ("Right", MouseButton::Right),
    ("Middle", MouseButton::Middle),
    ("Back", MouseButton::Back),
    ("Forward", MouseButton::Forward),
];

/// Parses a binding such as `key:Space`, `mouse:Left` or `pad:South`.
pub fn parse_button(text: &str) -> Option<Button> {
    let (device, name) = text.split_once(':')?;
    match device {
        "key" => KEYS
            .iter()
            .find(|(n, _)| *n == name)
            .map(|&(_, k)| Button::Key(k)),
        "mouse" => MOUSE_BUTTONS
            .iter()
            .find(|(n, _)| *n == name)
            .map(|&(_, b)| Button::Mouse(b))
            .or_else(|| {
                name.strip_prefix("Other")?
                    .parse()
                    .ok()
                    .map(|n| Button::Mouse(MouseButton::Other(n)))
            }),
        "pad" => PAD_BUTTONS
            .iter()
            .find(|(n, _)| *n == name)
            .map(|&(_, b)| Button::Pad(b)),
        _ => None,
    }
}

/// The binding text of a button (the inverse of [`parse_button`]).
pub fn button_name(button: Button) -> String {
    match button {
        Button::Key(k) => format!("key:{k:?}"),
        Button::Mouse(MouseButton::Other(n)) => format!("mouse:Other{n}"),
        Button::Mouse(m) => format!("mouse:{m:?}"),
        Button::Pad(b) => format!("pad:{b:?}"),
    }
}

/// Parses a stick name (`left` or `right`).
pub fn parse_stick(text: &str) -> Option<Stick> {
    match text {
        "left" => Some(Stick::Left),
        "right" => Some(Stick::Right),
        _ => None,
    }
}

/// Every valid binding text, for suggestions in diagnostics.
fn all_button_names() -> impl Iterator<Item = String> {
    KEYS.iter()
        .map(|(n, _)| format!("key:{n}"))
        .chain(MOUSE_BUTTONS.iter().map(|(n, _)| format!("mouse:{n}")))
        .chain(PAD_BUTTONS.iter().map(|(n, _)| format!("pad:{n}")))
}

/// A likely intended binding for a name that does not parse: the same name in
/// another case, a bare letter or digit (`key:w` is `key:KeyW`), or the closest
/// name by edit distance when it is close enough to be a typo.
pub(crate) fn suggest(text: &str) -> Option<String> {
    if let Some((device, name)) = text.split_once(':') {
        let mut chars = name.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            let guess = match c {
                'a'..='z' | 'A'..='Z' => format!("{device}:Key{}", c.to_ascii_uppercase()),
                '0'..='9' => format!("{device}:Digit{c}"),
                _ => String::new(),
            };
            if parse_button(&guess).is_some() {
                return Some(guess);
            }
        }
    }
    if let Some(exact) = all_button_names().find(|n| n.eq_ignore_ascii_case(text)) {
        return Some(exact);
    }
    closest(text, all_button_names())
}

/// The closest candidate to `text` by edit distance, if it is close enough to
/// be a likely typo.
fn closest(text: &str, candidates: impl Iterator<Item = String>) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    candidates
        .map(|c| (levenshtein(&lower, &c.to_ascii_lowercase()), c))
        .min_by_key(|(d, _)| *d)
        .filter(|(d, c)| *d <= (c.len() / 3).max(2))
        .map(|(_, c)| c)
}

fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = (prev + usize::from(ca != cb)).min(row[j] + 1).min(cur + 1);
            prev = cur;
        }
    }
    row[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_names_match_winit_debug_names() {
        assert_eq!(KEYS.len(), 194);
        for (name, key) in KEYS {
            assert_eq!(*name, format!("{key:?}"));
        }
        for (name, b) in PAD_BUTTONS {
            assert_eq!(*name, format!("{b:?}"));
        }
    }

    #[test]
    fn round_trip() {
        for text in [
            "key:Space",
            "key:KeyW",
            "mouse:Left",
            "mouse:Other7",
            "pad:South",
            "pad:DPadUp",
        ] {
            let b = parse_button(text).unwrap_or_else(|| panic!("{text}"));
            assert_eq!(button_name(b), text);
        }
        assert_eq!(parse_button("key:Spcae"), None);
        assert_eq!(parse_button("Space"), None);
        assert_eq!(suggest("key:Spcae").as_deref(), Some("key:Space"));
        assert_eq!(suggest("key:w").as_deref(), Some("key:KeyW"));
        assert_eq!(suggest("key:1").as_deref(), Some("key:Digit1"));
        assert_eq!(suggest("pad:south").as_deref(), Some("pad:South"));
        assert_eq!(suggest("banana:Hat"), None);
    }
}
