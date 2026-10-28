//! The diagnostic codes `re_agent` owns (range `RE8000`–`RE8099`).
//!
//! Each code has an explain page in `crates/re_agent/explain/` and an entry in
//! [`DIAGNOSTICS`](crate::DIAGNOSTICS). A code is never reused for a different
//! meaning (ADR-0004).

use re_core::Code;

/// Another live process holds the project's single-writer lock
/// (`.real/editor.lock`).
pub const PROJECT_LOCKED: Code = Code::new("RE8003");

/// The editor socket is not available on this platform (Windows, until the
/// named pipe of ADR-0020's Windows amendment).
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

/// `resource.read` named a URI that is not in the resource set.
pub const UNKNOWN_RESOURCE: Code = Code::new("RE8014");

/// A Luau tool script under `scripts/tools/commands/` could not be loaded or
/// does not return a valid tool; it is not registered.
pub const INVALID_TOOL: Code = Code::new("RE8015");

/// A Luau tool failed in a way only tools can: it changed the world without
/// `command.run` (the change was reverted).
pub const TOOL_FAILED: Code = Code::new("RE8016");

/// A line a Luau tool printed (a note in the call's outcome).
pub const TOOL_OUTPUT: Code = Code::new("RE8017");

/// `compat.coverage` was asked for a class, enum or library the coverage
/// table does not have.
pub const UNKNOWN_COMPAT_NAME: Code = Code::new("RE8018");

/// A template or recipe in the library is malformed (ADR-0046): bad TOML, a
/// file name that is not its name, a missing render file, an unsafe path.
pub const INVALID_TEMPLATE: Code = Code::new("RE8019");

/// A template, recipe or parameter name is unknown, or a required parameter
/// has no value.
pub const UNKNOWN_TEMPLATE: Code = Code::new("RE8020");

/// A parameter value has the wrong type or is out of its range.
pub const BAD_PARAM_VALUE: Code = Code::new("RE8021");

/// A recipe step names a command that is unknown or above the edit tier.
pub const RECIPE_STEP_REFUSED: Code = Code::new("RE8022");

/// A recipe step failed; an atomic apply restored the checkpoint.
pub const RECIPE_STEP_FAILED: Code = Code::new("RE8023");

/// A recipe or template check did not hold.
pub const RECIPE_CHECK_FAILED: Code = Code::new("RE8024");

/// An evaluation task or suite is malformed, or names a project that
/// cannot be found.
pub const INVALID_EVAL_TASK: Code = Code::new("RE8025");

/// `real agent eval --agent NAME` names no agent in any `eval.toml`, or the
/// file does not parse.
pub const UNKNOWN_EVAL_AGENT: Code = Code::new("RE8026");

/// An evaluation run could not be set up or its files written: the project
/// copy, the MCP server, or the output directory.
pub const EVAL_RUN_FAILED: Code = Code::new("RE8027");
