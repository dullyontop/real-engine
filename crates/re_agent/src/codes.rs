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

/// A call needs a higher tier than the agent policy allows.
pub const TIER_REFUSED: Code = Code::new("RE8001");

/// A file path is outside the agent policy's scopes, escapes the project or
/// names `.real/` or `.git/`.
pub const OUTSIDE_SCOPE: Code = Code::new("RE8002");

/// A call asks for more than the agent policy's budgets allow.
pub const OVER_BUDGET: Code = Code::new("RE8008");

/// `.real/agent-policy.toml` does not parse.
pub const INVALID_POLICY: Code = Code::new("RE8009");

/// A session or checkpoint file could not be read or written.
pub const CHECKPOINT_IO: Code = Code::new("RE8010");

/// No session or checkpoint of that id.
pub const UNKNOWN_CHECKPOINT: Code = Code::new("RE8011");

/// `checkpoint.restore` was called with no agent session.
pub const NO_SESSION: Code = Code::new("RE8012");

/// A file command's target is missing, not a file, or could not be written.
pub const FILE_ERROR: Code = Code::new("RE8013");
