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

/// A pose easing style or direction name is not one of
/// [`PoseStyle`](crate::PoseStyle)'s or [`PoseDirection`](crate::PoseDirection)'s.
pub const UNKNOWN_POSE_EASING: Code = Code::new("RE7603");

/// A keyframe clip has a negative or non-finite time, or a joint key with a
/// non-finite or zero rotation, a non-finite translation, or a bad weight.
pub const KEYFRAME_CLIP_MALFORMED: Code = Code::new("RE7604");

/// An animation priority name is not one of [`Priority`](crate::Priority)'s.
pub const UNKNOWN_PRIORITY: Code = Code::new("RE7605");
