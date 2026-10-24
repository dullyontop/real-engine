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

/// A skin cannot be evaluated: its joint, inverse bind and bound lists differ
/// in length, a joint's parent does not come before it, or it has more than
/// 65,535 joints (ADR-0039).
pub const SKIN_MALFORMED: Code = Code::new("RE7606");

/// An animation graph is not valid: a wrong format or version, a duplicate
/// id, a reference to a missing parameter, node, state or mask, a parameter of
/// the wrong type, or a value out of range. The message names the element by
/// its path in the file (ADR-0039).
pub const GRAPH_INVALID: Code = Code::new("RE7607");

/// A clip an animation graph's node plays cannot be loaded or sampled.
pub const GRAPH_CLIP_MISSING: Code = Code::new("RE7608");

/// A script or command named a graph parameter the graph does not have, or
/// gave it a value of the wrong type. The message lists the graph's
/// parameters.
pub const GRAPH_PARAM_UNKNOWN: Code = Code::new("RE7609");

/// [`GRAPH_PARAM_UNKNOWN`] as text, for `HostError`s.
pub const GRAPH_PARAM_UNKNOWN_STR: &str = "RE7609";

/// A joint mask names a joint the skeleton does not have (a warning: one mask
/// can serve several skeletons).
pub const GRAPH_MASK_JOINT_MISSING: Code = Code::new("RE7610");

/// An animation graph's blend tree has a cycle: a node is its own descendant.
pub const GRAPH_CYCLE: Code = Code::new("RE7611");
