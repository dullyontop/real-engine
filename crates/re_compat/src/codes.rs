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

/// A character rig could not be built: a rig data file or a
/// `HumanoidDescription` is malformed (WP14 holds RE9200–RE9249).
pub const INVALID_RIG: Code = Code::new("RE9200");

/// An `Animation.AnimationId` resolves to no clip; the track plays as an
/// empty clip.
pub const UNKNOWN_ANIMATION: Code = Code::new("RE9201");

/// `[compat.animations]` in `real.toml` has a key that is not an asset id, a
/// value that is not a built-in clip or a dotted path, or one id twice.
pub const BAD_ANIMATION_MAP: Code = Code::new("RE9202");

/// A member that reads online data (a player's avatar) returned an offline
/// default instead; the script goes on with it.
pub const OFFLINE_DEFAULT: Code = Code::new("RE9203");

/// A collision group operation was refused: the group is not registered, the
/// name is invalid or taken, all groups are in use, or `Default` was to be
/// removed or renamed (WP13 holds RE9250–RE9299).
pub const COLLISION_GROUP: Code = Code::new("RE9250");

/// A physics member needs something the place lacks: a `Model` without a
/// `PrimaryPart`, a workspace without its `Terrain`.
pub const PHYSICS_SETUP: Code = Code::new("RE9251");

/// An asset id (a sound, image or texture) is not in the project's
/// substitute table; it plays or draws nothing (WP15 holds RE9350–RE9399).
pub const UNKNOWN_ASSET: Code = Code::new("RE9350");

/// More particles, beam segments or trail points than the frame's effect
/// budget; the excess was not drawn.
pub const EFFECT_BUDGET: Code = Code::new("RE9351");

/// `TweenService:Create` was given a property whose type cannot be tweened.
pub const NOT_TWEENABLE: Code = Code::new("RE9352");

/// A data store request was refused: a bad name, key or value, a value too
/// large, a request from a client, or data access turned off. The message
/// starts with the vendor's error number (WP27 holds RE9400–RE9449).
pub const DATA_REQUEST: Code = Code::new("RE9400");

/// A data store request found its request type's queue full and was dropped.
pub const DATA_QUEUE_FULL: Code = Code::new("RE9401");

/// A data file under `.real/data/` (or `data/seed/`) did not read or write;
/// an unreadable file is set aside as `<file>.bad` and the store starts empty.
pub const DATA_FILE: Code = Code::new("RE9402");

/// A memory store request was refused: over the request units or the memory
/// quota, a value or structure too large, or a bad argument.
pub const MEMORY_REQUEST: Code = Code::new("RE9403");

/// A `SurfaceGui` or `ViewportFrame` cannot be drawn as asked: a surface
/// with no part to sit on, or a canvas larger than a texture can hold, drawn
/// scaled down (ADR-0048 §2).
pub const SURFACE: Code = Code::new("RE9410");

/// A compat script changed on disk during play but does not compile; the
/// running version stays until a save that compiles (ADR-0048 §3).
pub const RELOAD_COMPILE: Code = Code::new("RE9420");

/// A skinned `MeshPart`'s `Bone`s and its model's joints do not match: a
/// bone names no joint of the skin, or a joint has no bone, so that joint
/// stays in its rest pose (ADR-0048 §5). Once per part.
pub const BONE_MISMATCH: Code = Code::new("RE9440");

/// A remote message from a network client was dropped: larger than 64 KiB,
/// or not a message this build reads (WP30 holds RE9450–RE9499).
pub const REMOTE_DROPPED: Code = Code::new("RE9450");

/// A client changed a component of an instance it owns that does not flow to
/// the server (`[compat.net] upstream`); the change stays on that client.
pub const OWNED_WRITE: Code = Code::new("RE9451");

/// An owned part moved faster than `[compat.net] max_owned_speed`; the server
/// kept it where it was and sent it back to its owner.
pub const OWNED_SPEED: Code = Code::new("RE9452");

/// A network player sent more remote messages a second than
/// `[compat.net] remote_rate`; the excess was dropped.
pub const REMOTE_RATE: Code = Code::new("RE9453");

/// An `UnreliableRemoteEvent` message was too large for one datagram and was
/// dropped.
pub const UNRELIABLE_SIZE: Code = Code::new("RE9454");

/// `real verify`'s `net` stage: a replay played by a server and a client
/// over loopback in lockstep did not end where the in-process run did, or
/// the two could not run.
pub const NET_DIFFERS: Code = Code::new("RE9455");
