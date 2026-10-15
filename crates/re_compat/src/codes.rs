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

/// A value passed through a remote or bindable cannot be copied: a function,
/// a thread, a cyclic table, a key that is neither a string nor a number, or
/// a userdata that is not an instance, datatype or enum item (WP12 holds
/// RE9300–RE9349).
pub const REMOTE_VALUE: Code = Code::new("RE9300");

/// A remote or bindable call failed: its callback raised, no callback is set
/// (`BindableFunction:Invoke`), it was called from the wrong side, or the
/// player it was waiting on left.
pub const REMOTE_CALL: Code = Code::new("RE9301");

/// A host step about players was refused: the player is not a `Player` in
/// the game, or `[compat] players` is 0.
pub const PLAYER_STEP: Code = Code::new("RE9302");

/// A deprecated member was used; it works, and the note names its replacement.
pub const DEPRECATED_MEMBER: Code = Code::new("RE9101");

/// An asset id (a sound, image or texture) is not in the project's
/// substitute table; it plays or draws nothing (WP15 holds RE9350–RE9399).
pub const UNKNOWN_ASSET: Code = Code::new("RE9350");

/// More particles, beam segments or trail points than the frame's effect
/// budget; the excess was not drawn.
pub const EFFECT_BUDGET: Code = Code::new("RE9351");

/// `TweenService:Create` was given a property whose type cannot be tweened.
pub const NOT_TWEENABLE: Code = Code::new("RE9352");
