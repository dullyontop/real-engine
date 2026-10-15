//! The diagnostic codes `re_agent` owns (range `RE8000`–`RE8099`).
//!
//! Each code has an explain page in `crates/re_agent/explain/` and an entry in
//! [`DIAGNOSTICS`](crate::DIAGNOSTICS). A code is never reused for a different
//! meaning (ADR-0004).

use re_core::Code;

/// Another live process holds the project's single-writer lock
/// (`.real/editor.lock`).
pub const PROJECT_LOCKED: Code = Code::new("RE8003");

/// The editor socket is not available on this platform (Windows in M1).
pub const SOCKET_UNSUPPORTED: Code = Code::new("RE8004");

/// The editor socket could not be bound or connected, or the connection was
/// lost.
pub const SOCKET_IO: Code = Code::new("RE8005");

/// A message on the editor socket is not valid protocol JSON.
pub const SOCKET_PROTOCOL: Code = Code::new("RE8006");

/// The lock file could not be created, read or written.
pub const LOCK_IO: Code = Code::new("RE8007");
