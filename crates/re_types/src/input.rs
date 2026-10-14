//! The input a frame sees, after the action map.
//!
//! `re_input` turns raw device input (or a replay) into one [`ActionFrame`] per
//! frame. Scripts and gameplay read actions by name (`jump`, `move`), never keys.

use std::collections::BTreeMap;

use glam::Vec2;
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
