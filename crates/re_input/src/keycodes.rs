//! Button names for raw input events ([`re_types::RawInputFrame`]).
//!
//! Raw events name buttons as `real.toml` binds them, without the device
//! prefix ([`crate::names`]): a winit key code for a key (`KeyW`), a winit
//! mouse button for a mouse button (`Left`, or `Other4`) and a gilrs button
//! for a gamepad (`South`).

use re_types::input::InputKind;

use crate::names::{button_name, parse_button};
use crate::raw::Button;

/// The device kind and name of a button, as a raw event reports them.
pub fn describe(button: Button) -> (InputKind, String) {
    let text = button_name(button);
    let (_, name) = text
        .split_once(':')
        .expect("binding names have a device prefix");
    let kind = match button {
        Button::Key(_) => InputKind::Keyboard,
        Button::Mouse(_) => InputKind::MouseButton,
        Button::Pad(_) => InputKind::Gamepad,
    };
    (kind, name.to_string())
}

/// The key with winit name `name` (`KeyW`).
pub fn key(name: &str) -> Option<Button> {
    parse_button(&format!("key:{name}")).filter(|b| matches!(b, Button::Key(_)))
}

/// The mouse button named `name` (`Left`).
pub fn mouse_button(name: &str) -> Option<Button> {
    parse_button(&format!("mouse:{name}")).filter(|b| matches!(b, Button::Mouse(_)))
}

/// The gamepad button with gilrs name `name` (`South`).
pub fn pad_button(name: &str) -> Option<Button> {
    parse_button(&format!("pad:{name}")).filter(|b| matches!(b, Button::Pad(_)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::event::MouseButton;
    use winit::keyboard::KeyCode;

    #[test]
    fn names_round_trip() {
        for b in [
            Button::Key(KeyCode::KeyW),
            Button::Key(KeyCode::Space),
            Button::Mouse(MouseButton::Left),
            Button::Mouse(MouseButton::Other(4)),
            Button::Pad(gilrs::Button::South),
        ] {
            let (kind, name) = describe(b);
            let back = match kind {
                InputKind::Keyboard => key(&name),
                InputKind::MouseButton => mouse_button(&name),
                _ => pad_button(&name),
            };
            assert_eq!(back, Some(b), "{name}");
        }
        assert_eq!(key("Left"), None);
        assert_eq!(mouse_button("KeyW"), None);
    }
}
