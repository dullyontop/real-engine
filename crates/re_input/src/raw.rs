//! Raw device state, fed from winit and gilrs events or injected directly.

use std::collections::{HashMap, HashSet};

use glam::{Vec2, Vec3};
use re_types::input::{InputEvent, InputPhase, RawInputFrame};
use winit::event::{DeviceEvent, ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{KeyCode, PhysicalKey};

use crate::keycodes;

/// A physical control that reads as pressed or not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Button {
    /// A keyboard key, by physical position (WASD stays put on AZERTY).
    Key(KeyCode),
    /// A mouse button.
    Mouse(MouseButton),
    /// A gamepad button (any connected pad).
    Pad(gilrs::Button),
}

/// A gamepad stick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stick {
    /// The left stick (`LeftStickX`, `LeftStickY`).
    Left,
    /// The right stick (`RightStickX`, `RightStickY`).
    Right,
}

impl Stick {
    /// The gilrs axes of the stick, x then y.
    pub fn axes(self) -> (gilrs::Axis, gilrs::Axis) {
        match self {
            Stick::Left => (gilrs::Axis::LeftStickX, gilrs::Axis::LeftStickY),
            Stick::Right => (gilrs::Axis::RightStickX, gilrs::Axis::RightStickY),
        }
    }
}

/// Raw device state for one frame.
///
/// Feed it events during the frame ([`RawInput::on_window_event`],
/// [`RawInput::on_device_event`], [`RawInput::on_gamepad_event`]) or inject
/// state directly ([`RawInput::set_button`], [`RawInput::set_pad_axis`],
/// [`RawInput::add_mouse_delta`]) from tests, tools and AI agents. Read it
/// through an [`ActionMap`](crate::ActionMap), or as raw events with
/// [`RawInput::raw_frame`], then call [`RawInput::end_frame`].
#[derive(Debug, Clone)]
pub struct RawInput {
    down: HashSet<Button>,
    pressed_this_frame: HashSet<Button>,
    released_this_frame: HashSet<Button>,
    pad_axes: HashMap<gilrs::Axis, f32>,
    mouse_delta: (f64, f64),
    cursor: Option<(f64, f64)>,
    /// The last known cursor position, kept while the cursor is outside.
    last_cursor: (f64, f64),
    wheel: f32,
    events: Vec<InputEvent>,
    keyboard: bool,
    mouse: bool,
    gamepad: bool,
}

impl Default for RawInput {
    /// Nothing held, with a keyboard and a mouse present and no gamepad.
    fn default() -> Self {
        RawInput {
            down: HashSet::new(),
            pressed_this_frame: HashSet::new(),
            released_this_frame: HashSet::new(),
            pad_axes: HashMap::new(),
            mouse_delta: (0.0, 0.0),
            cursor: None,
            last_cursor: (0.0, 0.0),
            wheel: 0.0,
            events: Vec::new(),
            keyboard: true,
            mouse: true,
            gamepad: false,
        }
    }
}

impl RawInput {
    /// Sets a button's state. Only transitions count: setting a held button down
    /// again (a key repeat) is not a new press.
    pub fn set_button(&mut self, button: Button, down: bool) {
        let changed = if down {
            let new = self.down.insert(button);
            if new {
                self.pressed_this_frame.insert(button);
            }
            new
        } else {
            let gone = self.down.remove(&button);
            if gone {
                self.released_this_frame.insert(button);
            }
            gone
        };
        if changed {
            let (input_type, key) = keycodes::describe(button);
            let position = match button {
                Button::Mouse(_) => self.cursor_vec(),
                _ => Vec3::ZERO,
            };
            self.events.push(InputEvent {
                phase: if down {
                    InputPhase::Began
                } else {
                    InputPhase::Ended
                },
                input_type: input_type.to_string(),
                key: key.to_string(),
                position,
                delta: Vec3::ZERO,
            });
        }
    }

    fn cursor_vec(&self) -> Vec3 {
        let (x, y) = self.cursor.unwrap_or(self.last_cursor);
        Vec3::new(x as f32, y as f32, self.wheel)
    }

    /// Moves the cursor to `(x, y)` in window pixels, reported as a mouse
    /// movement event.
    pub fn set_cursor(&mut self, x: f64, y: f64) {
        if self.cursor == Some((x, y)) {
            return;
        }
        let (px, py) = self.cursor.unwrap_or(self.last_cursor);
        self.cursor = Some((x, y));
        self.last_cursor = (x, y);
        self.events.push(InputEvent {
            phase: InputPhase::Changed,
            input_type: "MouseMovement".to_string(),
            key: keycodes::UNKNOWN.to_string(),
            position: self.cursor_vec(),
            delta: Vec3::new((x - px) as f32, (y - py) as f32, 0.0),
        });
    }

    /// Turns the mouse wheel by `notches` (positive away from the user),
    /// reported as a wheel event.
    pub fn add_wheel(&mut self, notches: f32) {
        if !notches.is_finite() || notches == 0.0 {
            return;
        }
        self.wheel += notches;
        self.events.push(InputEvent {
            phase: InputPhase::Changed,
            input_type: "MouseWheel".to_string(),
            key: keycodes::UNKNOWN.to_string(),
            position: self.cursor_vec(),
            delta: Vec3::new(0.0, 0.0, notches),
        });
    }

    /// Says which devices are present (reported in [`RawInputFrame`]).
    pub fn set_devices(&mut self, keyboard: bool, mouse: bool, gamepad: bool) {
        self.keyboard = keyboard;
        self.mouse = mouse;
        self.gamepad = gamepad;
    }

    /// Sets a gamepad axis value, in `[-1, 1]` (+y is up on sticks).
    pub fn set_pad_axis(&mut self, axis: gilrs::Axis, value: f32) {
        self.pad_axes.insert(
            axis,
            if value.is_finite() {
                value.clamp(-1.0, 1.0)
            } else {
                0.0
            },
        );
    }

    /// Adds raw mouse motion, in device units (screen y grows downward).
    pub fn add_mouse_delta(&mut self, dx: f64, dy: f64) {
        if !(dx.is_finite() && dy.is_finite()) || (dx == 0.0 && dy == 0.0) {
            return;
        }
        self.mouse_delta.0 += dx;
        self.mouse_delta.1 += dy;
        self.events.push(InputEvent {
            phase: InputPhase::Changed,
            input_type: "MouseMovement".to_string(),
            key: keycodes::UNKNOWN.to_string(),
            position: self.cursor_vec(),
            delta: Vec3::new(dx as f32, dy as f32, 0.0),
        });
    }

    /// True while the button is held.
    pub fn is_down(&self, button: Button) -> bool {
        self.down.contains(&button)
    }

    /// True when the button went down this frame.
    pub fn was_pressed(&self, button: Button) -> bool {
        self.pressed_this_frame.contains(&button)
    }

    /// True when the button went up this frame.
    pub fn was_released(&self, button: Button) -> bool {
        self.released_this_frame.contains(&button)
    }

    /// A gamepad axis value (0 when never set).
    pub fn pad_axis(&self, axis: gilrs::Axis) -> f32 {
        self.pad_axes.get(&axis).copied().unwrap_or(0.0)
    }

    /// Mouse motion accumulated this frame.
    pub fn mouse_delta(&self) -> (f64, f64) {
        self.mouse_delta
    }

    /// The cursor position in window pixels, while it is over the window.
    pub fn cursor(&self) -> Option<(f64, f64)> {
        self.cursor
    }

    /// Feeds a winit window event. Returns true if it was an input event.
    pub fn on_window_event(&mut self, event: &WindowEvent) -> bool {
        match event {
            WindowEvent::KeyboardInput { event, .. } => {
                if let PhysicalKey::Code(code) = event.physical_key {
                    self.set_button(Button::Key(code), event.state == ElementState::Pressed);
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                self.set_button(Button::Mouse(*button), *state == ElementState::Pressed);
            }
            WindowEvent::CursorMoved { position, .. } => self.set_cursor(position.x, position.y),
            WindowEvent::MouseWheel { delta, .. } => match delta {
                MouseScrollDelta::LineDelta(_, y) => self.add_wheel(*y),
                // A notch is about 120 pixels of smooth scrolling on most platforms.
                MouseScrollDelta::PixelDelta(p) => self.add_wheel((p.y / 120.0) as f32),
            },
            WindowEvent::CursorLeft { .. } => self.cursor = None,
            // Releases that happen while unfocused never arrive; drop everything held.
            WindowEvent::Focused(false) => self.release_all(),
            _ => return false,
        }
        true
    }

    /// Feeds a winit device event: raw mouse motion (unaccelerated, and still
    /// reported with a grabbed cursor), used for camera look.
    pub fn on_device_event(&mut self, event: &DeviceEvent) {
        if let DeviceEvent::MouseMotion { delta } = event {
            self.add_mouse_delta(delta.0, delta.1);
        }
    }

    /// Feeds a gilrs event. gilrs events cannot be built outside gilrs (its
    /// `ev::Code` has a private field), so tests and replays use
    /// [`RawInput::set_button`] and [`RawInput::set_pad_axis`] instead.
    pub fn on_gamepad_event(&mut self, event: &gilrs::EventType) {
        match *event {
            gilrs::EventType::ButtonPressed(b, _) => self.set_button(Button::Pad(b), true),
            gilrs::EventType::ButtonReleased(b, _) => self.set_button(Button::Pad(b), false),
            gilrs::EventType::AxisChanged(a, v, _) => self.set_pad_axis(a, v),
            gilrs::EventType::Connected => self.gamepad = true,
            gilrs::EventType::Disconnected => {
                self.gamepad = false;
                self.pad_axes.clear();
                let pads: Vec<_> = self
                    .down
                    .iter()
                    .copied()
                    .filter(|b| matches!(b, Button::Pad(_)))
                    .collect();
                pads.into_iter().for_each(|b| self.set_button(b, false));
            }
            _ => {}
        }
    }

    /// Releases every held button (each counts as released this frame).
    pub fn release_all(&mut self) {
        let held: Vec<_> = self.down.iter().copied().collect();
        held.into_iter().for_each(|b| self.set_button(b, false));
    }

    /// The raw device state and this frame's events, in the order they
    /// happened, named as [`crate::keycodes`] describes.
    pub fn raw_frame(&self, frame: u64) -> RawInputFrame {
        let mut keys_down: Vec<String> = self
            .down
            .iter()
            .filter_map(|b| match b {
                Button::Key(_) | Button::Pad(_) => Some(keycodes::describe(*b).1),
                Button::Mouse(_) => None,
            })
            .filter(|n| *n != keycodes::UNKNOWN)
            .map(str::to_string)
            .collect();
        keys_down.sort();
        keys_down.dedup();
        let mut mouse_buttons_down: Vec<u8> = self
            .down
            .iter()
            .filter_map(|b| match b {
                Button::Mouse(m) => keycodes::mouse_button_number(*m),
                _ => None,
            })
            .collect();
        mouse_buttons_down.sort_unstable();
        let (x, y) = self.cursor.unwrap_or(self.last_cursor);
        RawInputFrame {
            frame,
            events: self.events.clone(),
            keys_down,
            mouse_buttons_down,
            mouse_position: Vec2::new(x as f32, y as f32),
            mouse_delta: Vec2::new(self.mouse_delta.0 as f32, self.mouse_delta.1 as f32),
            keyboard: self.keyboard,
            mouse: self.mouse,
            gamepad: self.gamepad,
        }
    }

    /// Clears the per-frame state (presses, releases, mouse motion, events).
    /// Call after the frame's actions have been read.
    pub fn end_frame(&mut self) {
        self.pressed_this_frame.clear();
        self.released_this_frame.clear();
        self.mouse_delta = (0.0, 0.0);
        self.events.clear();
    }
}

/// A gamepad connection, opened with [`Gamepads::new`].
///
/// A thin wrapper over gilrs that drains its events into a [`RawInput`]. When
/// gilrs cannot start (no backend on the platform), it stays empty.
pub struct Gamepads {
    gilrs: Option<gilrs::Gilrs>,
}

impl Gamepads {
    /// Opens gilrs. Failure is logged and gives an empty connection.
    pub fn new() -> Gamepads {
        match gilrs::Gilrs::new() {
            Ok(g) => Gamepads { gilrs: Some(g) },
            Err(gilrs::Error::NotImplemented(g)) => {
                tracing::info!("gamepads are not supported on this platform");
                Gamepads { gilrs: Some(g) }
            }
            Err(e) => {
                tracing::warn!(error = %e, "gamepad support is unavailable");
                Gamepads { gilrs: None }
            }
        }
    }

    /// The number of connected gamepads.
    pub fn connected(&self) -> usize {
        self.gilrs.as_ref().map_or(0, |g| g.gamepads().count())
    }

    /// Feeds every pending gamepad event into `raw`.
    pub fn poll(&mut self, raw: &mut RawInput) {
        if let Some(g) = &mut self.gilrs {
            while let Some(ev) = g.next_event() {
                raw.on_gamepad_event(&ev.event);
            }
        }
    }
}

impl Default for Gamepads {
    fn default() -> Self {
        Gamepads::new()
    }
}
