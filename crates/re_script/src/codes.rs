//! The diagnostic codes `re_script` owns: `RE3000`–`RE3079` for scripts and the
//! runtime, and `RE9xxx` for the compatibility block, which `re_script` holds in
//! M1 (ARCHITECTURE §7). `re_script_check` uses `RE3080`–`RE3099`.
//!
//! Each code has an explain page in `crates/re_script/explain/` and an entry in
//! [`DIAGNOSTICS`](crate::DIAGNOSTICS). A code is never reused for a different
//! meaning (ADR-0004).

use re_core::Code;

/// The source does not compile (a Luau syntax error).
pub const SYNTAX: Code = Code::new("RE3001");

/// A script raised an error while running.
pub const RUNTIME: Code = Code::new("RE3002");

/// A script ran past its context's CPU budget and was stopped.
pub const TIMEOUT: Code = Code::new("RE3010");

/// A script context ran out of its memory limit.
pub const MEMORY: Code = Code::new("RE3011");
