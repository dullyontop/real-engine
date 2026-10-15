//! `re_input`: raw input, the action map and replays for Real Engine.
//!
//! Games and scripts read *actions* (`jump`, `move`), never keys. The pieces:
//!
//! * [`RawInput`] holds device state, fed from winit window and device events
//!   (keys by physical position, mouse buttons, raw mouse motion for camera look)
//!   and from gilrs ([`Gamepads`]). Tests, tools and AI agents inject state with
//!   [`RawInput::set_button`] and [`RawInput::set_pad_axis`].
//! * [`ActionMap`] binds actions to buttons and axis sources. It is built from
//!   [`InputConfig`], the `[input]` section of `real.toml`, and gives one
//!   [`ActionFrame`] per frame.
//! * [`RawInput::raw_frame`] gives the frame's raw events and held keys as a
//!   [`RawInputFrame`], with keys named as [`keycodes`] lists, for layers that
//!   read keys rather than actions (the compatibility layer's input services).
//! * [`ReplayRecorder`] records frames into a [`ReplayDoc`]; [`Replay`] plays one
//!   back, giving the same frames. Replays carry actions and, in their `raw`
//!   list, raw key and mouse changes.
//!
//! A frame of live input:
//!
//! ```
//! use re_input::{Button, InputConfig, RawInput};
//! use winit::keyboard::KeyCode;
//!
//! let toml = r#"
//! [input.buttons]
//! jump = ["key:Space", "pad:South"]
//! "#;
//! let map = InputConfig::from_real_toml(toml, "real.toml").unwrap().to_action_map().unwrap();
//! let mut raw = RawInput::default();
//! raw.set_button(Button::Key(KeyCode::Space), true);
//! let frame = map.frame(&raw, 0);
//! assert!(frame.pressed("jump"));
//! raw.end_frame();
//! assert!(!map.frame(&raw, 1).pressed("jump"));
//! ```
//!
//! Errors are [`re_core::Diagnostic`]s with codes RE7200–RE7299; each has an
//! explain page listed in [`DIAGNOSTICS`].

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod codes;
mod config;
pub mod keycodes;
mod map;
pub mod names;
mod raw;
mod replay;

pub use config::{AxisSourceConfig, DEFAULT_DEAD_ZONE, InputConfig};
pub use map::{ActionLayout, ActionMap, AxisSource};
pub use raw::{Button, Gamepads, RawInput, Stick};
pub use re_types::input::{ActionFrame, ButtonState, InputEvent, InputPhase, RawInputFrame};
pub use re_types::replay::ReplayDoc;
pub use replay::{Replay, ReplayRecorder, load_replay, save_replay};

use re_core::DiagInfo;

/// Every diagnostic code this crate uses, with its explain page.
pub const DIAGNOSTICS: &[DiagInfo] = &[
    DiagInfo {
        code: codes::CONFIG_INVALID,
        title: "Invalid input configuration",
        page: include_str!("../explain/RE7200.md"),
    },
    DiagInfo {
        code: codes::UNKNOWN_BINDING,
        title: "Unknown input binding",
        page: include_str!("../explain/RE7201.md"),
    },
    DiagInfo {
        code: codes::AXIS_SOURCE_INVALID,
        title: "Invalid axis source",
        page: include_str!("../explain/RE7202.md"),
    },
    DiagInfo {
        code: codes::ACTION_KIND_CONFLICT,
        title: "Action is both a button and an axis",
        page: include_str!("../explain/RE7203.md"),
    },
    DiagInfo {
        code: codes::REPLAY_INVALID,
        title: "Invalid replay",
        page: include_str!("../explain/RE7204.md"),
    },
    DiagInfo {
        code: codes::REPLAY_UNKNOWN_ACTION,
        title: "Replay names an unknown action",
        page: include_str!("../explain/RE7205.md"),
    },
    DiagInfo {
        code: codes::REPLAY_IO,
        title: "Replay file cannot be read or written",
        page: include_str!("../explain/RE7206.md"),
    },
    DiagInfo {
        code: codes::REPLAY_PARSE,
        title: "Replay file is not valid",
        page: include_str!("../explain/RE7207.md"),
    },
];

#[cfg(test)]
mod tests {
    #[test]
    fn catalogue_is_valid() {
        let problems = re_core::diag::validate_catalog(super::DIAGNOSTICS, 7200..=7299);
        assert!(problems.is_empty(), "{problems:#?}");
    }
}
