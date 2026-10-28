//! The replay file, `tests/replays/*.replay.json` (ARCHITECTURE §8).
//!
//! A replay drives a headless run: input per frame, engine commands at given
//! frames, and checks (screenshots compared with per-backend goldens).
//!
//! ```json
//! { "$schema": "../../schemas/replay.schema.json", "version": 1, "seed": 1234, "frames": 600,
//!   "input": [ { "frame": 30, "action": "move", "axis": [0, 1] }, { "frame": 90, "action": "jump", "down": true } ],
//!   "commands": [ { "frame": 300, "name": "graph.regenerate", "args": { "source": "island", "seed": 99 } } ],
//!   "checks": [ { "frame": 600, "screenshot": "tests/goldens/{backend}/island_600.png" } ],
//!   "scenes": [ { "frame": 240, "switch": "town" } ] }
//! ```
//!
//! Unknown keys are errors.

use serde::{Deserialize, Serialize};

/// A replay document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ReplayDoc {
    /// An optional JSON Schema reference, kept unchanged.
    #[serde(rename = "$schema", default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    /// The file format version, [`ReplayDoc::VERSION`].
    pub version: u32,
    /// The seed the run starts with.
    pub seed: i64,
    /// The number of frames to run.
    pub frames: u64,
    /// Input changes, in frame order. An action keeps its last value until changed.
    #[serde(default)]
    pub input: Vec<ReplayInput>,
    /// Raw key and mouse changes, in frame order, for layers that read keys
    /// rather than actions (the compatibility layer's input services).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub raw: Vec<RawReplayInput>,
    /// Engine commands to dispatch, in frame order.
    #[serde(default)]
    pub commands: Vec<ReplayCommand>,
    /// Checks to make, in frame order.
    #[serde(default)]
    pub checks: Vec<ReplayCheck>,
    /// The scene changes the run must make, in order (ADR-0059 §6): checked
    /// as the run applies them, never driven. Without it, nothing is checked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scenes: Option<Vec<SceneChange>>,
}

impl ReplayDoc {
    /// The current file format version.
    pub const VERSION: u32 = 1;

    /// An empty replay of `frames` frames.
    pub fn new(seed: i64, frames: u64) -> ReplayDoc {
        ReplayDoc {
            schema: None,
            version: Self::VERSION,
            seed,
            frames,
            input: Vec::new(),
            raw: Vec::new(),
            commands: Vec::new(),
            checks: Vec::new(),
            scenes: None,
        }
    }
}

/// A change of one action's value at a frame.
///
/// Exactly one of `down` (a button action) and `axis` (an axis action) is set.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ReplayInput {
    /// The frame at which the value changes.
    pub frame: u64,
    /// The action name.
    pub action: String,
    /// The new button state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub down: Option<bool>,
    /// The new axis value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub axis: Option<[f32; 2]>,
}

impl ReplayInput {
    /// True when exactly one of `down` and `axis` is set.
    pub fn is_well_formed(&self) -> bool {
        self.down.is_some() != self.axis.is_some()
    }
}

/// One raw device change in a replay: a key, mouse button or gamepad button
/// going down or up, the cursor moving, or the wheel turning. Exactly one of
/// `key`, `mouse_button`, `pad_button`, `mouse_move`, `mouse_position` and
/// `wheel` is set; the three buttons also need `down`. Buttons are named as
/// `real.toml` binds them, without the device prefix (`KeyW`, `Left`,
/// `South`).
///
/// ```json
/// { "frame": 30, "key": "KeyW", "down": true }
/// { "frame": 31, "mouse_move": [12, -4] }
/// ```
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RawReplayInput {
    /// The frame the change happens on.
    pub frame: u64,
    /// A keyboard key (a winit key code: `KeyW`, `Space`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// A mouse button (`Left`, `Right`, `Middle`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mouse_button: Option<String>,
    /// A gamepad button (a gilrs button: `South`, `Start`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pad_button: Option<String>,
    /// For a button: down or up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub down: Option<bool>,
    /// Mouse motion in pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mouse_move: Option<[f32; 2]>,
    /// The cursor's new position in window pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mouse_position: Option<[f32; 2]>,
    /// A wheel turn, in notches (positive away from the user).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wheel: Option<f32>,
}

impl RawReplayInput {
    /// True when exactly one change is set, and `down` is set exactly for a
    /// button.
    pub fn is_well_formed(&self) -> bool {
        let set = [
            self.key.is_some(),
            self.mouse_button.is_some(),
            self.pad_button.is_some(),
            self.mouse_move.is_some(),
            self.mouse_position.is_some(),
            self.wheel.is_some(),
        ]
        .iter()
        .filter(|b| **b)
        .count();
        let button = self.key.is_some() || self.mouse_button.is_some() || self.pad_button.is_some();
        set == 1 && button == self.down.is_some()
    }
}

/// One scene change a run applied (ADR-0059 §6), at the end of `frame`: the
/// main scene switched, a scene loaded beside the loaded ones or unloaded, or
/// a restart into the scene the run began in. Exactly one of `switch`,
/// `load`, `unload` and `restart` is set, to the scene's name (its project
/// path when the project's scene list does not name it).
///
/// ```json
/// { "frame": 120, "switch": "town" }
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SceneChange {
    /// The frame whose end applied the change.
    pub frame: u64,
    /// The scene switched to: it replaced every loaded scene.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub switch: Option<String>,
    /// A scene loaded beside the loaded ones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load: Option<String>,
    /// A scene unloaded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unload: Option<String>,
    /// The scene a restart (`world.restart()`) loaded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart: Option<String>,
}

/// What a [`SceneChange`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SceneChangeKind {
    /// `switch`.
    Switch,
    /// `load`.
    Load,
    /// `unload`.
    Unload,
    /// `restart`.
    Restart,
}

impl SceneChangeKind {
    /// The change's key in a replay: `switch`, `load`, `unload` or `restart`.
    pub fn name(self) -> &'static str {
        match self {
            SceneChangeKind::Switch => "switch",
            SceneChangeKind::Load => "load",
            SceneChangeKind::Unload => "unload",
            SceneChangeKind::Restart => "restart",
        }
    }
}

impl SceneChange {
    /// The change `kind` of scene `name` at `frame`.
    pub fn new(frame: u64, kind: SceneChangeKind, name: impl Into<String>) -> SceneChange {
        let name = Some(name.into());
        let mut change = SceneChange {
            frame,
            ..SceneChange::default()
        };
        match kind {
            SceneChangeKind::Switch => change.switch = name,
            SceneChangeKind::Load => change.load = name,
            SceneChangeKind::Unload => change.unload = name,
            SceneChangeKind::Restart => change.restart = name,
        }
        change
    }

    /// What the change did and to which scene, when exactly one of the four
    /// keys is set.
    pub fn kind(&self) -> Option<(SceneChangeKind, &str)> {
        let set: Vec<(SceneChangeKind, &str)> = [
            (SceneChangeKind::Switch, &self.switch),
            (SceneChangeKind::Load, &self.load),
            (SceneChangeKind::Unload, &self.unload),
            (SceneChangeKind::Restart, &self.restart),
        ]
        .into_iter()
        .filter_map(|(k, n)| n.as_deref().map(|n| (k, n)))
        .collect();
        match set.as_slice() {
            [one] => Some(*one),
            _ => None,
        }
    }

    /// True when exactly one of `switch`, `load`, `unload` and `restart` is set.
    pub fn is_well_formed(&self) -> bool {
        self.kind().is_some()
    }
}

/// An engine command (from the command registry) dispatched at a frame.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ReplayCommand {
    /// The frame at which it is dispatched (before the frame's fixed steps).
    pub frame: u64,
    /// The command name, for example `graph.regenerate`.
    pub name: String,
    /// The command's arguments.
    #[serde(default = "empty_object")]
    pub args: serde_json::Value,
}

/// A check made at the end of a frame.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ReplayCheck {
    /// The frame after which the check runs.
    pub frame: u64,
    /// Capture the frame and compare it with this golden image. `{backend}` is
    /// replaced by the backend the run uses (`vulkan` or `gl`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screenshot: Option<String>,
}

fn empty_object() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}
