//! The diagnostic codes `re_core` owns (range `RE0000`–`RE0499`).
//!
//! Each code has an explain page in `crates/re_core/explain/` and an entry in
//! [`DIAGNOSTICS`](crate::DIAGNOSTICS). A code is never reused for a different
//! meaning (ADR-0004).

use crate::diag::Code;

/// The log filter (from `LogConfig::filter`) is not a valid `tracing` directive list.
pub const LOG_FILTER_INVALID: Code = Code::new("RE0001");

/// [`init_logging`](crate::init_logging) was called after a global subscriber was set.
pub const LOG_ALREADY_INITIALISED: Code = Code::new("RE0002");

/// The operating system refused to start the worker threads of a [`TaskPool`](crate::TaskPool).
pub const TASK_POOL_BUILD_FAILED: Code = Code::new("RE0003");

/// [`TaskPool::init_global`](crate::TaskPool::init_global) was called after the
/// global pool already existed.
pub const TASK_POOL_ALREADY_INITIALISED: Code = Code::new("RE0004");
