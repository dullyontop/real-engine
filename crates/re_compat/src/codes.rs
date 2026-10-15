//! The diagnostic codes `re_compat` owns (range `RE9002`–`RE9799`).
//!
//! Each code has an explain page in `crates/re_compat/explain/` and an entry in
//! [`DIAGNOSTICS`](crate::DIAGNOSTICS). A code is never reused for a different
//! meaning (ADR-0004). An unknown or unimplemented member raises `re_script`'s
//! [`UNSUPPORTED_MEMBER`](re_script::codes::UNSUPPORTED_MEMBER), which every
//! layer shares.

use re_core::Code;

/// A member that needs an online service (data stores, teleport, the
/// marketplace and the like) was called; it is not available offline.
pub const NOT_AVAILABLE_OFFLINE: Code = Code::new("RE9002");

/// A class, enum or library data file is malformed or inconsistent.
pub const INVALID_DATA: Code = Code::new("RE9003");

/// A compat scene breaks the scene conventions (no `game` root, an unknown
/// class, a service outside `game`, a script whose context does not match its
/// class).
pub const SCENE_CONVENTION: Code = Code::new("RE9004");

/// A script wrote a read-only property.
pub const READ_ONLY: Code = Code::new("RE9005");

/// `SetAttribute` refused a name or a value type (WP11 holds RE9006–RE9019).
pub const INVALID_ATTRIBUTE: Code = Code::new("RE9007");

/// A `Parent` change was refused: the instance was destroyed (its parent is
/// locked), or the new parent is the instance itself or one of its
/// descendants.
pub const INVALID_PARENT: Code = Code::new("RE9008");

/// A deprecated member was used; it works, and the note names its replacement.
pub const DEPRECATED_MEMBER: Code = Code::new("RE9101");

/// A character rig could not be built: a rig data file or a
/// `HumanoidDescription` is malformed (WP14 holds RE9200–RE9249).
pub const INVALID_RIG: Code = Code::new("RE9200");

/// An `Animation.AnimationId` resolves to no clip; the track plays as an
/// empty clip.
pub const UNKNOWN_ANIMATION: Code = Code::new("RE9201");

/// `[compat.animations]` in `real.toml` has a key that is not an asset id, a
/// value that is not a built-in clip or a dotted path, or one id twice.
pub const BAD_ANIMATION_MAP: Code = Code::new("RE9202");

/// An asset id (a sound, image or texture) is not in the project's
/// substitute table; it plays or draws nothing (WP15 holds RE9350–RE9399).
pub const UNKNOWN_ASSET: Code = Code::new("RE9350");

/// More particles, beam segments or trail points than the frame's effect
/// budget; the excess was not drawn.
pub const EFFECT_BUDGET: Code = Code::new("RE9351");

/// `TweenService:Create` was given a property whose type cannot be tweened.
pub const NOT_TWEENABLE: Code = Code::new("RE9352");
