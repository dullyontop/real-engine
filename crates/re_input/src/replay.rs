//! Replays: recorded input that drives a run frame by frame.
//!
//! A replay stores action changes, so it does not depend on keyboard layouts or
//! connected devices. [`ReplayRecorder`] turns a run's [`ActionFrame`]s into a
//! [`ReplayDoc`]; [`Replay`] plays it back, giving the same frames.
//!
//! Layers that read keys rather than actions (the compatibility layer's input
//! services) also need raw device changes. Those go in the document's `raw`
//! list ([`RawReplayInput`]): keys and mouse buttons going down or up, mouse
//! motion, the cursor position and the wheel. [`Replay::raw_frame`] gives them
//! back as a [`RawInputFrame`] per frame, and [`ReplayRecorder::record_raw`]
//! records them.

use std::collections::BTreeMap;
use std::path::Path;

use glam::Vec2;
use re_core::{Diagnostic, Location};
use re_types::input::{ActionFrame, ButtonState, InputPhase, RawInputFrame};
use re_types::replay::{RawReplayInput, ReplayCheck, ReplayCommand, ReplayDoc, ReplayInput};

use crate::codes;
use crate::keycodes;
use crate::map::ActionLayout;
use crate::raw::{Button, RawInput};

/// Plays a [`ReplayDoc`] back as one [`ActionFrame`] per frame.
#[derive(Debug, Clone)]
pub struct Replay {
    doc: ReplayDoc,
    layout: ActionLayout,
    next_input: usize,
    frame: u64,
    down: BTreeMap<String, bool>,
    axes: BTreeMap<String, Vec2>,
    next_raw: usize,
    raw: RawInput,
    raw_frame: RawInputFrame,
}

impl Replay {
    /// Checks `doc` against the game's actions and prepares playback from frame 0.
    ///
    /// Every frame lists every action of `layout`. With an empty layout, the
    /// actions are those the replay mentions.
    ///
    /// # Errors
    ///
    /// Every problem found: [`codes::REPLAY_INVALID`] for a wrong version, an
    /// entry with both or neither of `down` and `axis`, entries out of frame order
    /// or past the end, and [`codes::REPLAY_UNKNOWN_ACTION`] for an action the
    /// layout does not have (or one used as both a button and an axis).
    pub fn new(doc: ReplayDoc, layout: &ActionLayout) -> Result<Replay, Vec<Diagnostic>> {
        let mut problems = Vec::new();
        let invalid = |msg: String| Diagnostic::error(codes::REPLAY_INVALID, msg);
        if doc.version != ReplayDoc::VERSION {
            problems.push(invalid(format!(
                "replay version {} is not supported (this engine reads version {})",
                doc.version,
                ReplayDoc::VERSION
            )));
        }
        let mut layout = layout.clone();
        let derive = layout.buttons.is_empty() && layout.axes.is_empty();
        let mut last = 0;
        for (i, input) in doc.input.iter().enumerate() {
            let entry = format!(
                "input entry {} (frame {}, action `{}`)",
                i + 1,
                input.frame,
                input.action
            );
            if input.frame < last {
                problems.push(invalid(format!(
                    "{entry} comes after an entry for frame {last}; entries must be in frame order"
                )));
            }
            last = last.max(input.frame);
            if input.frame >= doc.frames {
                problems.push(invalid(format!(
                    "{entry} is past the end of the replay ({} frames)",
                    doc.frames
                )));
            }
            if !input.is_well_formed() {
                problems.push(invalid(format!(
                    "{entry} needs exactly one of `down` and `axis`"
                )));
                continue;
            }
            let is_button = input.down.is_some();
            let (own, other) = if is_button {
                (&mut layout.buttons, &layout.axes)
            } else {
                (&mut layout.axes, &layout.buttons)
            };
            if other.contains(&input.action) {
                problems.push(Diagnostic::error(
                    codes::REPLAY_UNKNOWN_ACTION,
                    format!(
                        "{entry} uses `{}` as {}, but it is {}",
                        input.action,
                        if is_button { "a button" } else { "an axis" },
                        if is_button { "an axis" } else { "a button" },
                    ),
                ));
            } else if !own.contains(&input.action) {
                if derive {
                    own.push(input.action.clone());
                    own.sort();
                } else {
                    problems.push(
                        Diagnostic::error(
                            codes::REPLAY_UNKNOWN_ACTION,
                            format!("{entry} names an action that `[input]` does not define"),
                        )
                        .with_hint("add the action to real.toml, or re-record the replay"),
                    );
                }
            }
        }
        let mut last_raw = 0;
        for (i, r) in doc.raw.iter().enumerate() {
            let entry = format!("raw entry {} (frame {})", i + 1, r.frame);
            if r.frame < last_raw {
                problems.push(invalid(format!(
                    "{entry} comes after an entry for frame {last_raw}; entries must be in frame order"
                )));
            }
            last_raw = last_raw.max(r.frame);
            if r.frame >= doc.frames {
                problems.push(invalid(format!(
                    "{entry} is past the end of the replay ({} frames)",
                    doc.frames
                )));
            }
            if !r.is_well_formed() {
                problems.push(invalid(format!(
                    "{entry} needs exactly one of `key`, `mouse_button`, `mouse_move`, \
                     `mouse_position` and `wheel`, with `down` for a key or mouse button"
                )));
                continue;
            }
            if let Some(k) = &r.key
                && keycodes::button_by_name(k).is_none()
            {
                problems.push(
                    invalid(format!("{entry} names the unknown key `{k}`"))
                        .with_hint("keys are named like `W`, `Space`, `LeftShift` or `ButtonA`"),
                );
            }
            if let Some(b) = r.mouse_button
                && keycodes::mouse_button_by_number(b).is_none()
            {
                problems.push(invalid(format!(
                    "{entry} names mouse button {b}; buttons are 1 (left), 2 (right) and 3 (middle)"
                )));
            }
        }
        for (what, frames) in [
            (
                "command",
                doc.commands.iter().map(|c| c.frame).collect::<Vec<_>>(),
            ),
            ("check", doc.checks.iter().map(|c| c.frame).collect()),
        ] {
            if frames.windows(2).any(|w| w[0] > w[1]) {
                problems.push(invalid(format!("{what} entries must be in frame order")));
            }
        }
        if !problems.is_empty() {
            return Err(problems);
        }
        let down = layout.buttons.iter().map(|a| (a.clone(), false)).collect();
        let axes = layout
            .axes
            .iter()
            .map(|a| (a.clone(), Vec2::ZERO))
            .collect();
        Ok(Replay {
            doc,
            layout,
            next_input: 0,
            frame: 0,
            down,
            axes,
            next_raw: 0,
            raw: RawInput::default(),
            raw_frame: RawInputFrame::default(),
        })
    }

    /// The replay document.
    pub fn doc(&self) -> &ReplayDoc {
        &self.doc
    }

    /// The actions every frame lists.
    pub fn layout(&self) -> &ActionLayout {
        &self.layout
    }

    /// The frame the next call to [`Replay::next_frame`] gives.
    pub fn frame(&self) -> u64 {
        self.frame
    }

    /// True once every frame has been played.
    pub fn is_finished(&self) -> bool {
        self.frame >= self.doc.frames
    }

    /// The next frame's actions, or `None` after the last frame.
    pub fn next_frame(&mut self) -> Option<ActionFrame> {
        if self.is_finished() {
            return None;
        }
        let f = self.frame;
        let mut went_down: BTreeMap<&str, bool> = BTreeMap::new();
        let mut went_up: BTreeMap<&str, bool> = BTreeMap::new();
        while let Some(input) = self.doc.input.get(self.next_input).filter(|i| i.frame == f) {
            if let Some(d) = input.down {
                let cur = self.down.entry(input.action.clone()).or_default();
                if *cur != d {
                    *cur = d;
                    let edges = if d { &mut went_down } else { &mut went_up };
                    edges.insert(&input.action, true);
                }
            } else if let Some([x, y]) = input.axis {
                self.axes.insert(input.action.clone(), Vec2::new(x, y));
            }
            self.next_input += 1;
        }
        while let Some(r) = self.doc.raw.get(self.next_raw).filter(|r| r.frame == f) {
            apply_raw(&mut self.raw, r);
            self.next_raw += 1;
        }
        self.raw_frame = self.raw.raw_frame(f);
        self.raw.end_frame();
        let buttons = self
            .down
            .iter()
            .map(|(a, &down)| {
                let state = ButtonState {
                    down,
                    pressed: went_down.contains_key(a.as_str()),
                    // Matches the action map: an action that went up and down again
                    // within the frame is still held, so it is not "released".
                    released: went_up.contains_key(a.as_str()) && !down,
                };
                (a.clone(), state)
            })
            .collect();
        self.frame += 1;
        Some(ActionFrame {
            frame: f,
            buttons,
            axes: self.axes.clone(),
        })
    }

    /// The raw device state and events of the frame [`Replay::next_frame`]
    /// last returned (empty before the first frame).
    pub fn raw_frame(&self) -> &RawInputFrame {
        &self.raw_frame
    }

    /// The commands to dispatch at `frame`, in order.
    pub fn commands_at(&self, frame: u64) -> impl Iterator<Item = &ReplayCommand> {
        self.doc.commands.iter().filter(move |c| c.frame == frame)
    }

    /// The checks to make after `frame`, in order.
    pub fn checks_at(&self, frame: u64) -> impl Iterator<Item = &ReplayCheck> {
        self.doc.checks.iter().filter(move |c| c.frame == frame)
    }
}

/// Records a run's [`ActionFrame`]s as a [`ReplayDoc`].
///
/// Only changes are stored. Playing the result with [`Replay`] gives frames equal
/// to the recorded ones (for the actions the recorded frames list).
#[derive(Debug, Clone)]
pub struct ReplayRecorder {
    doc: ReplayDoc,
    down: BTreeMap<String, bool>,
    axes: BTreeMap<String, Vec2>,
}

impl ReplayRecorder {
    /// An empty recording for a run started with `seed`.
    pub fn new(seed: i64) -> ReplayRecorder {
        ReplayRecorder {
            doc: ReplayDoc::new(seed, 0),
            down: BTreeMap::new(),
            axes: BTreeMap::new(),
        }
    }

    /// The number of frames recorded so far (the next frame's number).
    pub fn frames(&self) -> u64 {
        self.doc.frames
    }

    /// Records the next frame. Frames are numbered from 0 by the recorder; the
    /// `frame` field of `actions` is ignored.
    pub fn record(&mut self, actions: &ActionFrame) {
        let f = self.doc.frames;
        for (action, state) in &actions.buttons {
            let prev = self.down.get(action).copied().unwrap_or(false);
            let mut push = |down: bool| {
                self.doc.input.push(ReplayInput {
                    frame: f,
                    action: action.clone(),
                    down: Some(down),
                    axis: None,
                });
            };
            if state.pressed {
                // A press: up first if it was held, then down, then up again for a
                // tap shorter than a frame.
                if prev {
                    push(false);
                }
                push(true);
                if !state.down {
                    push(false);
                }
            } else if state.down != prev {
                push(state.down);
            }
            self.down.insert(action.clone(), state.down);
        }
        for (action, &value) in &actions.axes {
            let prev = self.axes.get(action).copied().unwrap_or(Vec2::ZERO);
            if value != prev {
                self.doc.input.push(ReplayInput {
                    frame: f,
                    action: action.clone(),
                    down: None,
                    axis: Some(value.to_array()),
                });
            }
            self.axes.insert(action.clone(), value);
        }
        self.doc.frames += 1;
    }

    /// Adds an engine command to dispatch at `frame`.
    pub fn command(&mut self, frame: u64, name: impl Into<String>, args: serde_json::Value) {
        self.doc.commands.push(ReplayCommand {
            frame,
            name: name.into(),
            args,
        });
        self.doc.commands.sort_by_key(|c| c.frame);
    }

    /// Records the raw key and mouse changes of the frame about to be recorded
    /// with [`ReplayRecorder::record`] (call this first): every key and mouse
    /// button that went down or up, the mouse motion, the cursor position when
    /// it moved, and wheel turns.
    pub fn record_raw(&mut self, raw: &RawInputFrame) {
        let frame = self.doc.frames;
        let row = || RawReplayInput {
            frame,
            ..RawReplayInput::default()
        };
        let mut moved = false;
        for e in &raw.events {
            let down = match e.phase {
                InputPhase::Began => Some(true),
                InputPhase::Ended => Some(false),
                InputPhase::Changed => None,
            };
            let mouse_button = match e.input_type.as_str() {
                "MouseButton1" => Some(1),
                "MouseButton2" => Some(2),
                "MouseButton3" => Some(3),
                _ => None,
            };
            match (e.input_type.as_str(), down) {
                (_, Some(down)) if mouse_button.is_some() => self.doc.raw.push(RawReplayInput {
                    mouse_button,
                    down: Some(down),
                    ..row()
                }),
                (_, Some(down)) if e.key != keycodes::UNKNOWN => {
                    self.doc.raw.push(RawReplayInput {
                        key: Some(e.key.clone()),
                        down: Some(down),
                        ..row()
                    })
                }
                ("MouseWheel", None) => self.doc.raw.push(RawReplayInput {
                    wheel: Some(e.delta.z),
                    ..row()
                }),
                ("MouseMovement", None) => moved = true,
                _ => {}
            }
        }
        if moved {
            // Motion and the final position, once per frame: a replay only
            // needs what the frame saw, not every intermediate event.
            if raw.mouse_delta != Vec2::ZERO {
                self.doc.raw.push(RawReplayInput {
                    mouse_move: Some(raw.mouse_delta.to_array()),
                    ..row()
                });
            }
            self.doc.raw.push(RawReplayInput {
                mouse_position: Some(raw.mouse_position.to_array()),
                ..row()
            });
        }
    }

    /// Adds a screenshot check after `frame`.
    pub fn screenshot(&mut self, frame: u64, golden: impl Into<String>) {
        self.doc.checks.push(ReplayCheck {
            frame,
            screenshot: Some(golden.into()),
        });
        self.doc.checks.sort_by_key(|c| c.frame);
    }

    /// The finished document.
    pub fn finish(self) -> ReplayDoc {
        self.doc
    }
}

/// Applies one raw replay row to `raw`.
fn apply_raw(raw: &mut RawInput, r: &RawReplayInput) {
    if let (Some(k), Some(down)) = (&r.key, r.down) {
        if let Some(b) = keycodes::button_by_name(k) {
            raw.set_button(b, down);
        }
    } else if let (Some(n), Some(down)) = (r.mouse_button, r.down) {
        if let Some(m) = keycodes::mouse_button_by_number(n) {
            raw.set_button(Button::Mouse(m), down);
        }
    } else if let Some([dx, dy]) = r.mouse_move {
        raw.add_mouse_delta(f64::from(dx), f64::from(dy));
    } else if let Some([x, y]) = r.mouse_position {
        raw.set_cursor(f64::from(x), f64::from(y));
    } else if let Some(w) = r.wheel {
        raw.add_wheel(w);
    }
}

/// Reads a replay file.
///
/// # Errors
///
/// [`codes::REPLAY_IO`] when the file cannot be read and [`codes::REPLAY_PARSE`]
/// when it is not a valid replay document (unknown keys are errors), located at
/// the line and column.
pub fn load_replay(path: &Path) -> Result<ReplayDoc, Diagnostic> {
    let file = path.to_string_lossy().replace('\\', "/");
    let text = std::fs::read_to_string(path).map_err(|e| {
        Diagnostic::error(codes::REPLAY_IO, format!("cannot read replay: {e}"))
            .at(Location::file(&file))
    })?;
    serde_json::from_str(&text).map_err(|e| {
        Diagnostic::error(codes::REPLAY_PARSE, format!("invalid replay: {e}"))
            .at(Location::file_at(&file, e.line() as u32, e.column() as u32))
    })
}

/// Writes a replay file as pretty JSON ending with a newline, through a temporary
/// file and a rename so a crash never leaves half a file.
///
/// # Errors
///
/// [`codes::REPLAY_IO`] when the file cannot be written.
pub fn save_replay(doc: &ReplayDoc, path: &Path) -> Result<(), Diagnostic> {
    let file = path.to_string_lossy().replace('\\', "/");
    let io = |e: std::io::Error| {
        Diagnostic::error(codes::REPLAY_IO, format!("cannot write replay: {e}"))
            .at(Location::file(&file))
    };
    let mut text = serde_json::to_string_pretty(doc).expect("a replay always serializes");
    text.push('\n');
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    std::fs::write(&tmp, text).map_err(io)?;
    std::fs::rename(&tmp, path).map_err(io)
}
