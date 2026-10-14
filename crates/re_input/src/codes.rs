//! The diagnostic codes `re_input` owns (range `RE7200`–`RE7299`).
//!
//! Each code has an explain page in `crates/re_input/explain/` and an entry in
//! [`DIAGNOSTICS`](crate::DIAGNOSTICS). A code is never reused for a different
//! meaning (ADR-0004).

use re_core::Code;

/// The `[input]` section of `real.toml` is not valid TOML or has unknown or
/// mistyped keys.
pub const CONFIG_INVALID: Code = Code::new("RE7200");

/// A binding names a key, mouse button or pad button that does not exist.
pub const UNKNOWN_BINDING: Code = Code::new("RE7201");

/// An axis source is malformed (no kind or several, a bad stick or dead zone).
pub const AXIS_SOURCE_INVALID: Code = Code::new("RE7202");

/// An action is defined both as a button and as an axis.
pub const ACTION_KIND_CONFLICT: Code = Code::new("RE7203");

/// A replay document is inconsistent (version, entry shape, frame order).
pub const REPLAY_INVALID: Code = Code::new("RE7204");

/// A replay names an action the game does not define, or uses a button as an axis.
pub const REPLAY_UNKNOWN_ACTION: Code = Code::new("RE7205");

/// A replay file cannot be read or written.
pub const REPLAY_IO: Code = Code::new("RE7206");

/// A replay file is not valid replay JSON.
pub const REPLAY_PARSE: Code = Code::new("RE7207");
