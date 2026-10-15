//! The input a frame sees, after the action map.
//!
//! `re_input` turns raw device input (or a replay) into one [`ActionFrame`] per
//! frame. Scripts and gameplay read actions by name (`jump`, `move`), never keys.

use std::collections::BTreeMap;

use glam::{Vec2, Vec3};
use serde::{Deserialize, Serialize};

/// The state of one button action this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ButtonState {
    /// Held down now.
    pub down: bool,
    /// Went down this frame.
    pub pressed: bool,
    /// Went up this frame.
    pub released: bool,
}

/// The state of every action this frame.
///
/// Maps are ordered by action name, so the serialized form is deterministic.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ActionFrame {
    /// The frame number (0 for the first frame of a run).
    pub frame: u64,
    /// Button actions (from `[input.buttons]` in `real.toml`).
    pub buttons: BTreeMap<String, ButtonState>,
    /// Axis actions (from `[input.axes]`), each a 2D value in `[-1, 1]` per
    /// component; a one-dimensional axis uses `x`. Look axes may exceed 1.
    pub axes: BTreeMap<String, Vec2>,
}

impl ActionFrame {
    /// The state of a button action; released and idle when the action is unknown.
    pub fn button(&self, action: &str) -> ButtonState {
        self.buttons.get(action).copied().unwrap_or_default()
    }

    /// True while the button action is held.
    pub fn is_down(&self, action: &str) -> bool {
        self.button(action).down
    }

    /// True on the frame the button action went down.
    pub fn pressed(&self, action: &str) -> bool {
        self.button(action).pressed
    }

    /// True on the frame the button action went up.
    pub fn released(&self, action: &str) -> bool {
        self.button(action).released
    }

    /// The value of an axis action; zero when the action is unknown.
    pub fn axis(&self, action: &str) -> Vec2 {
        self.axes.get(action).copied().unwrap_or(Vec2::ZERO)
    }
}

/// Where a raw [`InputEvent`] is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum InputPhase {
    /// A key or button went down, or a touch or text input started.
    Began,
    /// A held input changed: the mouse moved, the wheel turned, a stick moved.
    Changed,
    /// A key or button went up.
    Ended,
}

/// One raw device event of a frame, for layers that need keys rather than
/// actions (the compatibility layer's input services).
///
/// Names are strings, so this crate needs no key table: `input_type` is a
/// device kind (`Keyboard`, `MouseButton1`, `MouseButton2`, `MouseButton3`,
/// `MouseMovement`, `MouseWheel`, `Gamepad1`) and `key` a key or button name
/// (`W`, `Space`, `LeftShift`, `ButtonA`, `Thumbstick1`, or `Unknown`).
/// `re_input` documents the names it produces.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InputEvent {
    /// Began, changed or ended.
    pub phase: InputPhase,
    /// The device kind.
    pub input_type: String,
    /// The key or button, `Unknown` when the event has none.
    pub key: String,
    /// For mouse events, the cursor in window pixels (`x`, `y`) with the wheel
    /// position in `z`; for a stick, its value; zero otherwise.
    pub position: Vec3,
    /// For mouse movement, the motion in pixels; for the wheel, the turn in
    /// `z`; for a stick, the change; zero otherwise.
    pub delta: Vec3,
}

/// The raw device state of one frame, next to its [`ActionFrame`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RawInputFrame {
    /// The frame number.
    pub frame: u64,
    /// This frame's events, in the order they happened.
    pub events: Vec<InputEvent>,
    /// The keys and gamepad buttons held now, by name, sorted.
    pub keys_down: Vec<String>,
    /// The mouse buttons held now (1 left, 2 right, 3 middle), sorted.
    pub mouse_buttons_down: Vec<u8>,
    /// The cursor in window pixels.
    pub mouse_position: Vec2,
    /// The mouse motion this frame, in pixels.
    pub mouse_delta: Vec2,
    /// A keyboard is present.
    pub keyboard: bool,
    /// A mouse is present.
    pub mouse: bool,
    /// A gamepad is connected.
    pub gamepad: bool,
}

impl RawInputFrame {
    /// True while the key or gamepad button `name` is held.
    pub fn key_down(&self, name: &str) -> bool {
        self.keys_down.iter().any(|k| k == name)
    }

    /// True while mouse button `button` (1, 2 or 3) is held.
    pub fn mouse_button_down(&self, button: u8) -> bool {
        self.mouse_buttons_down.contains(&button)
    }
}

/// How the window treats the cursor, as a game asks for it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MouseLock {
    /// The cursor moves freely.
    #[default]
    Free,
    /// The cursor is held at the window centre; motion still arrives as deltas.
    LockCenter,
    /// The cursor stays where it is; motion still arrives as deltas.
    LockCurrentPosition,
}
