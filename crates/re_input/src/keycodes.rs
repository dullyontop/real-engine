//! Key names for raw input events ([`re_types::RawInputFrame`]).
//!
//! Raw events name keys and gamepad buttons the way the compatibility layer's
//! `KeyCode` enum does (`W`, `Space`, `LeftShift`, `ButtonA`), so the layer can
//! hand them to scripts without another table. The device kinds are named
//! like its `UserInputType` items (`Keyboard`, `MouseButton1`, `Gamepad1`).
//! A physical key with no such name is reported as `Unknown`.

use winit::event::MouseButton;
use winit::keyboard::KeyCode;

use crate::raw::Button;

/// The name of an event with no key.
pub const UNKNOWN: &str = "Unknown";

/// Every keyboard key that has a name, with that name. Keys are physical, so
/// on an AZERTY keyboard the key labelled Z is still `W`.
pub const KEYS: &[(KeyCode, &str)] = &[
    (KeyCode::Backspace, "Backspace"),
    (KeyCode::Tab, "Tab"),
    (KeyCode::Enter, "Return"),
    (KeyCode::Pause, "Pause"),
    (KeyCode::Escape, "Escape"),
    (KeyCode::Space, "Space"),
    (KeyCode::Quote, "Quote"),
    (KeyCode::Comma, "Comma"),
    (KeyCode::Minus, "Minus"),
    (KeyCode::Period, "Period"),
    (KeyCode::Slash, "Slash"),
    (KeyCode::Digit0, "Zero"),
    (KeyCode::Digit1, "One"),
    (KeyCode::Digit2, "Two"),
    (KeyCode::Digit3, "Three"),
    (KeyCode::Digit4, "Four"),
    (KeyCode::Digit5, "Five"),
    (KeyCode::Digit6, "Six"),
    (KeyCode::Digit7, "Seven"),
    (KeyCode::Digit8, "Eight"),
    (KeyCode::Digit9, "Nine"),
    (KeyCode::Semicolon, "Semicolon"),
    (KeyCode::Equal, "Equals"),
    (KeyCode::BracketLeft, "LeftBracket"),
    (KeyCode::Backslash, "BackSlash"),
    (KeyCode::BracketRight, "RightBracket"),
    (KeyCode::Backquote, "Backquote"),
    (KeyCode::KeyA, "A"),
    (KeyCode::KeyB, "B"),
    (KeyCode::KeyC, "C"),
    (KeyCode::KeyD, "D"),
    (KeyCode::KeyE, "E"),
    (KeyCode::KeyF, "F"),
    (KeyCode::KeyG, "G"),
    (KeyCode::KeyH, "H"),
    (KeyCode::KeyI, "I"),
    (KeyCode::KeyJ, "J"),
    (KeyCode::KeyK, "K"),
    (KeyCode::KeyL, "L"),
    (KeyCode::KeyM, "M"),
    (KeyCode::KeyN, "N"),
    (KeyCode::KeyO, "O"),
    (KeyCode::KeyP, "P"),
    (KeyCode::KeyQ, "Q"),
    (KeyCode::KeyR, "R"),
    (KeyCode::KeyS, "S"),
    (KeyCode::KeyT, "T"),
    (KeyCode::KeyU, "U"),
    (KeyCode::KeyV, "V"),
    (KeyCode::KeyW, "W"),
    (KeyCode::KeyX, "X"),
    (KeyCode::KeyY, "Y"),
    (KeyCode::KeyZ, "Z"),
    (KeyCode::Delete, "Delete"),
    (KeyCode::Numpad0, "KeypadZero"),
    (KeyCode::Numpad1, "KeypadOne"),
    (KeyCode::Numpad2, "KeypadTwo"),
    (KeyCode::Numpad3, "KeypadThree"),
    (KeyCode::Numpad4, "KeypadFour"),
    (KeyCode::Numpad5, "KeypadFive"),
    (KeyCode::Numpad6, "KeypadSix"),
    (KeyCode::Numpad7, "KeypadSeven"),
    (KeyCode::Numpad8, "KeypadEight"),
    (KeyCode::Numpad9, "KeypadNine"),
    (KeyCode::NumpadDecimal, "KeypadPeriod"),
    (KeyCode::NumpadDivide, "KeypadDivide"),
    (KeyCode::NumpadMultiply, "KeypadMultiply"),
    (KeyCode::NumpadSubtract, "KeypadMinus"),
    (KeyCode::NumpadAdd, "KeypadPlus"),
    (KeyCode::NumpadEnter, "KeypadEnter"),
    (KeyCode::NumpadEqual, "KeypadEquals"),
    (KeyCode::ArrowUp, "Up"),
    (KeyCode::ArrowDown, "Down"),
    (KeyCode::ArrowRight, "Right"),
    (KeyCode::ArrowLeft, "Left"),
    (KeyCode::Insert, "Insert"),
    (KeyCode::Home, "Home"),
    (KeyCode::End, "End"),
    (KeyCode::PageUp, "PageUp"),
    (KeyCode::PageDown, "PageDown"),
    (KeyCode::F1, "F1"),
    (KeyCode::F2, "F2"),
    (KeyCode::F3, "F3"),
    (KeyCode::F4, "F4"),
    (KeyCode::F5, "F5"),
    (KeyCode::F6, "F6"),
    (KeyCode::F7, "F7"),
    (KeyCode::F8, "F8"),
    (KeyCode::F9, "F9"),
    (KeyCode::F10, "F10"),
    (KeyCode::F11, "F11"),
    (KeyCode::F12, "F12"),
    (KeyCode::F13, "F13"),
    (KeyCode::F14, "F14"),
    (KeyCode::F15, "F15"),
    (KeyCode::NumLock, "NumLock"),
    (KeyCode::CapsLock, "CapsLock"),
    (KeyCode::ScrollLock, "ScrollLock"),
    (KeyCode::ShiftRight, "RightShift"),
    (KeyCode::ShiftLeft, "LeftShift"),
    (KeyCode::ControlRight, "RightControl"),
    (KeyCode::ControlLeft, "LeftControl"),
    (KeyCode::AltRight, "RightAlt"),
    (KeyCode::AltLeft, "LeftAlt"),
    (KeyCode::SuperLeft, "LeftSuper"),
    (KeyCode::SuperRight, "RightSuper"),
    (KeyCode::Help, "Help"),
    (KeyCode::PrintScreen, "Print"),
    (KeyCode::ContextMenu, "Menu"),
    (KeyCode::Power, "Power"),
    (KeyCode::Undo, "Undo"),
];

/// Every gamepad button that has a name, with that name.
pub const PAD_BUTTONS: &[(gilrs::Button, &str)] = &[
    (gilrs::Button::South, "ButtonA"),
    (gilrs::Button::East, "ButtonB"),
    (gilrs::Button::West, "ButtonX"),
    (gilrs::Button::North, "ButtonY"),
    (gilrs::Button::LeftTrigger, "ButtonL1"),
    (gilrs::Button::RightTrigger, "ButtonR1"),
    (gilrs::Button::LeftTrigger2, "ButtonL2"),
    (gilrs::Button::RightTrigger2, "ButtonR2"),
    (gilrs::Button::LeftThumb, "ButtonL3"),
    (gilrs::Button::RightThumb, "ButtonR3"),
    (gilrs::Button::Start, "ButtonStart"),
    (gilrs::Button::Select, "ButtonSelect"),
    (gilrs::Button::DPadUp, "DPadUp"),
    (gilrs::Button::DPadDown, "DPadDown"),
    (gilrs::Button::DPadLeft, "DPadLeft"),
    (gilrs::Button::DPadRight, "DPadRight"),
];

/// The name of a keyboard key, or [`UNKNOWN`].
pub fn key_name(code: KeyCode) -> &'static str {
    KEYS.iter()
        .find(|(c, _)| *c == code)
        .map_or(UNKNOWN, |(_, n)| n)
}

/// The keyboard key with this name.
pub fn key_by_name(name: &str) -> Option<KeyCode> {
    KEYS.iter().find(|(_, n)| *n == name).map(|(c, _)| *c)
}

/// The name of a gamepad button, or [`UNKNOWN`].
pub fn pad_button_name(button: gilrs::Button) -> &'static str {
    PAD_BUTTONS
        .iter()
        .find(|(b, _)| *b == button)
        .map_or(UNKNOWN, |(_, n)| n)
}

/// The gamepad button with this name.
pub fn pad_button_by_name(name: &str) -> Option<gilrs::Button> {
    PAD_BUTTONS
        .iter()
        .find(|(_, n)| *n == name)
        .map(|(b, _)| *b)
}

/// The number of a mouse button: 1 left, 2 right, 3 middle; `None` for the
/// others.
pub fn mouse_button_number(button: MouseButton) -> Option<u8> {
    match button {
        MouseButton::Left => Some(1),
        MouseButton::Right => Some(2),
        MouseButton::Middle => Some(3),
        _ => None,
    }
}

/// The mouse button with this number (1, 2 or 3).
pub fn mouse_button_by_number(n: u8) -> Option<MouseButton> {
    match n {
        1 => Some(MouseButton::Left),
        2 => Some(MouseButton::Right),
        3 => Some(MouseButton::Middle),
        _ => None,
    }
}

/// The button a key or button name stands for: a keyboard key first, then a
/// gamepad button.
pub fn button_by_name(name: &str) -> Option<Button> {
    key_by_name(name)
        .map(Button::Key)
        .or_else(|| pad_button_by_name(name).map(Button::Pad))
}

/// The device kind and key name of a button, as a raw event reports them.
pub fn describe(button: Button) -> (&'static str, &'static str) {
    match button {
        Button::Key(k) => ("Keyboard", key_name(k)),
        Button::Mouse(m) => (
            match mouse_button_number(m) {
                Some(1) => "MouseButton1",
                Some(2) => "MouseButton2",
                Some(3) => "MouseButton3",
                _ => "None",
            },
            UNKNOWN,
        ),
        Button::Pad(b) => ("Gamepad1", pad_button_name(b)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_and_round_trip() {
        let mut seen = std::collections::HashSet::new();
        for (code, name) in KEYS {
            assert!(seen.insert(*name), "duplicate key name {name}");
            assert_eq!(key_by_name(name), Some(*code));
            assert_eq!(key_name(*code), *name);
        }
        for (b, name) in PAD_BUTTONS {
            assert!(seen.insert(*name), "duplicate button name {name}");
            assert_eq!(pad_button_by_name(name), Some(*b));
        }
        assert_eq!(key_name(KeyCode::F30), UNKNOWN);
        for n in 1..=3 {
            assert_eq!(
                mouse_button_by_number(n).and_then(mouse_button_number),
                Some(n)
            );
        }
    }
}
