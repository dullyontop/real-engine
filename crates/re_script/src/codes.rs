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

/// A script or module file could not be read.
pub const SCRIPT_READ: Code = Code::new("RE3003");

/// `require` was refused or could not resolve the module.
pub const REQUIRE: Code = Code::new("RE3004");

/// `task.wait` was called where it cannot yield to the scheduler.
pub const YIELD: Code = Code::new("RE3005");

/// An engine function got an argument of the wrong type or shape.
pub const BAD_ARGUMENT: Code = Code::new("RE3006");

/// An entity handle was used after its entity was despawned.
pub const STALE_HANDLE: Code = Code::new("RE3007");

/// `world.define_component` was given a definition it cannot register.
pub const COMPONENT_DEFINITION: Code = Code::new("RE3008");

/// `command.run` was called in a context with no command registry.
pub const COMMANDS_UNAVAILABLE: Code = Code::new("RE3009");

/// A ProcGraph builder script built or returned an invalid graph.
pub const GRAPH_BUILDER: Code = Code::new("RE3012");

/// More `task.defer` calls ran in one cycle than the scheduler allows.
pub const DEFER_LIMIT: Code = Code::new("RE3014");

/// An engine service is not available in this context (no game is running).
pub const SERVICE_UNAVAILABLE: Code = Code::new("RE3015");

/// [`SERVICE_UNAVAILABLE`] as the `&'static str` a `HostError` carries.
pub const SERVICE_UNAVAILABLE_STR: &str = "RE3015";

/// A read that depends on frame timing or live output
/// (`Entity:drawn_transform`, `audio.levels`) was made during a fixed step.
pub const FRAME_ONLY: Code = Code::new("RE3026");

/// A script read a member the engine API does not have.
pub const UNSUPPORTED_MEMBER: Code = Code::new("RE9001");
