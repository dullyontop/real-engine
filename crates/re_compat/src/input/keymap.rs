//! Raw key names (`re_input`'s binding names: winit key codes such as
//! `KeyW`, gilrs gamepad buttons such as `South`) to `Enum.KeyCode` items.
//! A key with no item here reports as `KeyCode.None`.

/// Keyboard keys: winit name, `KeyCode` item.
pub const KEYS: &[(&str, &str)] = &[
    ("Backspace", "Backspace"),
    ("Tab", "Tab"),
    ("Enter", "Return"),
    ("Pause", "Pause"),
    ("Escape", "Escape"),
    ("Space", "Space"),
    ("Quote", "Quote"),
    ("Comma", "Comma"),
    ("Minus", "Minus"),
    ("Period", "Period"),
    ("Slash", "Slash"),
    ("Digit0", "Zero"),
    ("Digit1", "One"),
    ("Digit2", "Two"),
    ("Digit3", "Three"),
    ("Digit4", "Four"),
    ("Digit5", "Five"),
    ("Digit6", "Six"),
    ("Digit7", "Seven"),
    ("Digit8", "Eight"),
    ("Digit9", "Nine"),
    ("Semicolon", "Semicolon"),
    ("Equal", "Equals"),
    ("BracketLeft", "LeftBracket"),
    ("Backslash", "BackSlash"),
    ("BracketRight", "RightBracket"),
    ("Backquote", "Backquote"),
    ("KeyA", "A"),
    ("KeyB", "B"),
    ("KeyC", "C"),
    ("KeyD", "D"),
    ("KeyE", "E"),
    ("KeyF", "F"),
    ("KeyG", "G"),
    ("KeyH", "H"),
    ("KeyI", "I"),
    ("KeyJ", "J"),
    ("KeyK", "K"),
    ("KeyL", "L"),
    ("KeyM", "M"),
    ("KeyN", "N"),
    ("KeyO", "O"),
    ("KeyP", "P"),
    ("KeyQ", "Q"),
    ("KeyR", "R"),
    ("KeyS", "S"),
    ("KeyT", "T"),
    ("KeyU", "U"),
    ("KeyV", "V"),
    ("KeyW", "W"),
    ("KeyX", "X"),
    ("KeyY", "Y"),
    ("KeyZ", "Z"),
    ("Delete", "Delete"),
    ("Numpad0", "KeypadZero"),
    ("Numpad1", "KeypadOne"),
    ("Numpad2", "KeypadTwo"),
    ("Numpad3", "KeypadThree"),
    ("Numpad4", "KeypadFour"),
    ("Numpad5", "KeypadFive"),
    ("Numpad6", "KeypadSix"),
    ("Numpad7", "KeypadSeven"),
    ("Numpad8", "KeypadEight"),
    ("Numpad9", "KeypadNine"),
    ("NumpadDecimal", "KeypadPeriod"),
    ("NumpadDivide", "KeypadDivide"),
    ("NumpadMultiply", "KeypadMultiply"),
    ("NumpadSubtract", "KeypadMinus"),
    ("NumpadAdd", "KeypadPlus"),
    ("NumpadEnter", "KeypadEnter"),
    ("NumpadEqual", "KeypadEquals"),
    ("ArrowUp", "Up"),
    ("ArrowDown", "Down"),
    ("ArrowRight", "Right"),
    ("ArrowLeft", "Left"),
    ("Insert", "Insert"),
    ("Home", "Home"),
    ("End", "End"),
    ("PageUp", "PageUp"),
    ("PageDown", "PageDown"),
    ("F1", "F1"),
    ("F2", "F2"),
    ("F3", "F3"),
    ("F4", "F4"),
    ("F5", "F5"),
    ("F6", "F6"),
    ("F7", "F7"),
    ("F8", "F8"),
    ("F9", "F9"),
    ("F10", "F10"),
    ("F11", "F11"),
    ("F12", "F12"),
    ("F13", "F13"),
    ("F14", "F14"),
    ("F15", "F15"),
    ("NumLock", "NumLock"),
    ("CapsLock", "CapsLock"),
    ("ScrollLock", "ScrollLock"),
    ("ShiftRight", "RightShift"),
    ("ShiftLeft", "LeftShift"),
    ("ControlRight", "RightControl"),
    ("ControlLeft", "LeftControl"),
    ("AltRight", "RightAlt"),
    ("AltLeft", "LeftAlt"),
    ("SuperLeft", "LeftSuper"),
    ("SuperRight", "RightSuper"),
    ("Help", "Help"),
    ("PrintScreen", "Print"),
    ("ContextMenu", "Menu"),
    ("Power", "Power"),
    ("Undo", "Undo"),
];

/// Gamepad buttons: gilrs name, `KeyCode` item.
pub const PAD_BUTTONS: &[(&str, &str)] = &[
    ("South", "ButtonA"),
    ("East", "ButtonB"),
    ("West", "ButtonX"),
    ("North", "ButtonY"),
    ("LeftTrigger", "ButtonL1"),
    ("RightTrigger", "ButtonR1"),
    ("LeftTrigger2", "ButtonL2"),
    ("RightTrigger2", "ButtonR2"),
    ("LeftThumb", "ButtonL3"),
    ("RightThumb", "ButtonR3"),
    ("Start", "ButtonStart"),
    ("Select", "ButtonSelect"),
    ("DPadUp", "DPadUp"),
    ("DPadDown", "DPadDown"),
    ("DPadLeft", "DPadLeft"),
    ("DPadRight", "DPadRight"),
    ("LeftStick", "Thumbstick1"),
    ("RightStick", "Thumbstick2"),
];

/// The `KeyCode` item of a keyboard key name.
pub fn key_code(name: &str) -> Option<&'static str> {
    KEYS.iter().find(|(k, _)| *k == name).map(|(_, c)| *c)
}

/// The `KeyCode` item of a gamepad button name.
pub fn pad_code(name: &str) -> Option<&'static str> {
    PAD_BUTTONS
        .iter()
        .find(|(k, _)| *k == name)
        .map(|(_, c)| *c)
}

/// The raw key name of a `KeyCode` item: a keyboard key, else a gamepad
/// button.
pub fn raw_of(code: &str) -> Option<(bool, &'static str)> {
    KEYS.iter()
        .find(|(_, c)| *c == code)
        .map(|(k, _)| (true, *k))
        .or_else(|| {
            PAD_BUTTONS
                .iter()
                .find(|(_, c)| *c == code)
                .map(|(k, _)| (false, *k))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enums::EnumItem;

    #[test]
    fn every_mapped_name_is_a_key_code_item() {
        for (raw, code) in KEYS.iter().chain(PAD_BUTTONS) {
            assert!(
                EnumItem::lookup("KeyCode", code).is_some(),
                "{raw} -> {code}"
            );
        }
    }
}
