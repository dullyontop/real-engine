//! The action map: named actions bound to physical controls.

use std::collections::{BTreeMap, HashMap};
use std::hash::Hash;

use glam::Vec2;
use re_types::input::{ActionFrame, ButtonState};

use crate::raw::{Button, RawInput, Stick};

/// A source of a two-dimensional axis value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AxisSource {
    /// Four buttons (WASD, the d-pad): up, down, left, right. Diagonals are
    /// normalised, so they are not faster.
    Composite {
        /// Gives +y.
        up: Button,
        /// Gives −y.
        down: Button,
        /// Gives −x.
        left: Button,
        /// Gives +x.
        right: Button,
    },
    /// A gamepad stick with a radial dead zone. Output starts at 0 at the edge of
    /// the dead zone and reaches 1 at full tilt.
    Stick {
        /// Which stick.
        stick: Stick,
        /// The dead-zone radius in `[0, 1)`.
        dead_zone: f32,
    },
    /// This frame's raw mouse motion times `scale` (+y is up). Not clamped:
    /// look axes may exceed 1.
    MouseDelta {
        /// Units per device count.
        scale: f32,
    },
}

/// Action bindings. `A` is the game's action type: an enum in Rust code, or
/// `String` for actions defined in `real.toml` (see
/// [`InputConfig`](crate::InputConfig)).
///
/// One action may have many bindings, across devices. A button action is held
/// while any binding is held; it is pressed on the frame it becomes held and
/// released on the frame it stops being held.
#[derive(Debug, Clone)]
pub struct ActionMap<A = String> {
    buttons: HashMap<A, Vec<Button>>,
    axes: HashMap<A, Vec<AxisSource>>,
}

impl<A> Default for ActionMap<A> {
    fn default() -> Self {
        ActionMap {
            buttons: HashMap::new(),
            axes: HashMap::new(),
        }
    }
}

impl<A: Eq + Hash + Clone> ActionMap<A> {
    /// An empty map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a button binding to `action`.
    pub fn bind_button(&mut self, action: A, button: Button) -> &mut Self {
        self.buttons.entry(action).or_default().push(button);
        self
    }

    /// Adds an axis source to `action`.
    pub fn bind_axis(&mut self, action: A, source: AxisSource) -> &mut Self {
        self.axes.entry(action).or_default().push(source);
        self
    }

    /// Removes every binding of `action`.
    pub fn unbind(&mut self, action: &A) {
        self.buttons.remove(action);
        self.axes.remove(action);
    }

    /// The bindings of a button action.
    pub fn button_bindings(&self, action: &A) -> &[Button] {
        self.buttons.get(action).map_or(&[], Vec::as_slice)
    }

    /// The sources of an axis action.
    pub fn axis_sources(&self, action: &A) -> &[AxisSource] {
        self.axes.get(action).map_or(&[], Vec::as_slice)
    }

    /// The button actions, in no particular order.
    pub fn button_actions(&self) -> impl Iterator<Item = &A> {
        self.buttons.keys()
    }

    /// The axis actions, in no particular order.
    pub fn axis_actions(&self) -> impl Iterator<Item = &A> {
        self.axes.keys()
    }

    /// Held by any binding.
    pub fn held(&self, raw: &RawInput, action: &A) -> bool {
        self.button_bindings(action).iter().any(|&b| raw.is_down(b))
    }

    /// Became held this frame: a binding went down while no other binding was
    /// already held. A press and release within one frame still counts.
    pub fn just_pressed(&self, raw: &RawInput, action: &A) -> bool {
        let bs = self.button_bindings(action);
        let any_new = bs.iter().any(|&b| raw.was_pressed(b));
        let held_before = bs.iter().any(|&b| raw.is_down(b) && !raw.was_pressed(b));
        any_new && !held_before
    }

    /// Stopped being held this frame.
    pub fn just_released(&self, raw: &RawInput, action: &A) -> bool {
        self.button_bindings(action)
            .iter()
            .any(|&b| raw.was_released(b))
            && !self.held(raw, action)
    }

    /// The button state of `action` this frame.
    pub fn button(&self, raw: &RawInput, action: &A) -> ButtonState {
        ButtonState {
            down: self.held(raw, action),
            pressed: self.just_pressed(raw, action),
            released: self.just_released(raw, action),
        }
    }

    /// The sum of every source of `action`. Button sources together are clamped
    /// to the unit disc; analog sources are added on top. +y is up/forward.
    pub fn axis(&self, raw: &RawInput, action: &A) -> Vec2 {
        let mut digital = Vec2::ZERO;
        let mut analog = Vec2::ZERO;
        for src in self.axis_sources(action) {
            match *src {
                AxisSource::Composite {
                    up,
                    down,
                    left,
                    right,
                } => {
                    let v = |b| if raw.is_down(b) { 1.0 } else { 0.0 };
                    digital += Vec2::new(v(right) - v(left), v(up) - v(down));
                }
                AxisSource::Stick { stick, dead_zone } => {
                    let (ax, ay) = stick.axes();
                    let s = Vec2::new(raw.pad_axis(ax), raw.pad_axis(ay));
                    let len = s.length();
                    if len > dead_zone {
                        let k = ((len - dead_zone) / (1.0 - dead_zone)).min(1.0) / len;
                        analog += s * k;
                    }
                }
                AxisSource::MouseDelta { scale } => {
                    let (dx, dy) = raw.mouse_delta();
                    analog += Vec2::new(dx as f32 * scale, -(dy as f32) * scale);
                }
            }
        }
        digital.clamp_length_max(1.0) + analog
    }
}

impl<A: Eq + Hash + Clone + AsRef<str>> ActionMap<A> {
    /// Every action's state this frame, as the frame number `frame` (what scripts
    /// and gameplay read). Every bound action is listed, idle ones included.
    pub fn frame(&self, raw: &RawInput, frame: u64) -> ActionFrame {
        ActionFrame {
            frame,
            buttons: self
                .buttons
                .keys()
                .map(|a| (a.as_ref().to_owned(), self.button(raw, a)))
                .collect::<BTreeMap<_, _>>(),
            axes: self
                .axes
                .keys()
                .map(|a| (a.as_ref().to_owned(), self.axis(raw, a)))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    /// The names of the bound actions, for [`Replay`](crate::Replay) playback.
    pub fn layout(&self) -> ActionLayout {
        let mut buttons: Vec<String> = self.buttons.keys().map(|a| a.as_ref().to_owned()).collect();
        let mut axes: Vec<String> = self.axes.keys().map(|a| a.as_ref().to_owned()).collect();
        buttons.sort();
        axes.sort();
        ActionLayout { buttons, axes }
    }
}

/// The action names a game defines: which actions an [`ActionFrame`] lists.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActionLayout {
    /// Button action names, sorted.
    pub buttons: Vec<String>,
    /// Axis action names, sorted.
    pub axes: Vec<String>,
}
