//! The diagnostic codes `re_audio` owns (range `RE7400`–`RE7499`).
//!
//! Each code has an explain page in `crates/re_audio/explain/` and an entry in
//! [`DIAGNOSTICS`](crate::DIAGNOSTICS). A code is never reused for a different
//! meaning (ADR-0004).

use re_core::Code;

/// No audio output device could be opened; audio continues silently on the
/// headless backend (a warning).
pub const NO_OUTPUT_DEVICE: Code = Code::new("RE7400");

/// A command names a clip that is neither registered nor a file under the asset root.
pub const CLIP_NOT_FOUND: Code = Code::new("RE7401");

/// A clip file exists but cannot be decoded.
pub const CLIP_DECODE_FAILED: Code = Code::new("RE7402");

/// kira refused to start a sound or a spatial track (a capacity was reached).
pub const PLAYBACK_LIMIT: Code = Code::new("RE7403");

/// A command has a volume that is negative or not finite.
pub const INVALID_VOLUME: Code = Code::new("RE7404");

/// A playback rate is not a finite number above zero.
pub const INVALID_RATE: Code = Code::new("RE7405");

/// An effect in the audio graph, a snapshot or a command has an invalid value,
/// or names a parameter its kind does not have; the effect is dropped or left
/// unchanged.
pub const INVALID_EFFECT: Code = Code::new("RE7406");

/// The audio graph has an unknown parent or send, a cycle, too many sends, or
/// another structural problem; the default graph is used instead.
pub const INVALID_GRAPH: Code = Code::new("RE7407");

/// A command or snapshot names a bus, send or snapshot the graph does not have
/// (a warning, given once per name).
pub const UNKNOWN_BUS: Code = Code::new("RE7408");
