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

/// A deprecated member was used; it works, and the note names its replacement.
pub const DEPRECATED_MEMBER: Code = Code::new("RE9101");
