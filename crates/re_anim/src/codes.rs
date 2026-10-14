//! The diagnostic codes `re_anim` owns (range `RE7600`–`RE7699`).
//!
//! Each code has an explain page in `crates/re_anim/explain/` and an entry in
//! [`DIAGNOSTICS`](crate::DIAGNOSTICS). A code is never reused for a different
//! meaning (ADR-0004).

use re_core::Code;

/// A clip channel has keyframe times that are not strictly increasing, a value
/// count that does not match its interpolation, or a non-finite time.
pub const CLIP_MALFORMED: Code = Code::new("RE7600");

/// An easing name (from a script) is not one of [`Easing`](crate::Easing)'s names.
pub const UNKNOWN_EASING: Code = Code::new("RE7601");

/// A tween's duration is negative or not finite.
pub const TWEEN_INVALID: Code = Code::new("RE7602");
