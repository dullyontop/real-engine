//! The replay file, `tests/replays/*.replay.json` (ARCHITECTURE §8).
//!
//! A replay drives a headless run: input per frame, engine commands at given
//! frames, and checks (screenshots compared with per-backend goldens).
//!
//! ```json
//! { "$schema": "../../schemas/replay.schema.json", "version": 1, "seed": 1234, "frames": 600,
//!   "input": [ { "frame": 30, "action": "move", "axis": [0, 1] }, { "frame": 90, "action": "jump", "down": true } ],
//!   "commands": [ { "frame": 300, "name": "graph.regenerate", "args": { "source": "island", "seed": 99 } } ],
//!   "checks": [ { "frame": 600, "screenshot": "tests/goldens/{backend}/island_600.png" } ] }
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
    /// Engine commands to dispatch, in frame order.
    #[serde(default)]
    pub commands: Vec<ReplayCommand>,
    /// Checks to make, in frame order.
    #[serde(default)]
    pub checks: Vec<ReplayCheck>,
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
            commands: Vec::new(),
            checks: Vec::new(),
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
