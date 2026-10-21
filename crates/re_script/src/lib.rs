//! `re_script`: Luau scripting for Real Engine (ADR-0011, ADR-0012).
//!
//! Luau is embedded through mlua 0.12.2 (Luau 0.740) with the `luau` feature only.
//! This crate provides:
//!
//! * [`compile_check`]: compile a script without running it (validation tier 1,
//!   ADR-0024). **Frozen** from WP3's first PR; `re_agent`'s `script.write` and
//!   `re_script_check` call it.
//! * [`diag`]: [`ScriptError`], which unwraps mlua errors into the file, line,
//!   message and traceback, and maps them to `RE3xxx` [`Diagnostic`](re_core::Diagnostic)s.
//! * [`ScriptRuntime`]: the `game` and `tools` contexts (sandboxed VMs with the
//!   engine API, a CPU budget `pcall` cannot swallow, a memory limit and
//!   project-confined `require`), the per-phase entry point and hot reload.
//!   **Frozen** at WP3's exit; `re_app` drives it through a [`ScriptHost`].
//! * [`definitions`]: `engine.d.luau`, generated from the API registrations;
//!   [`write_definitions`] puts it at `.real/types/engine.d.luau` in a project.
//!
//! Diagnostic codes: `RE3000`–`RE3079` and the compatibility block `RE9xxx`
//! (ARCHITECTURE §7); see [`codes`].

#![warn(missing_docs)]

pub mod api;
mod budget;
pub mod codes;
mod compile;
pub mod debug;
pub mod diag;
pub mod host;
pub mod modules;
mod runtime;
mod scheduler;
pub mod vm;

/// The scheduler, for layers that run their own contexts on it
/// ([`Context::compat`]): simulation time, spawning and deferring threads,
/// and yielding host calls (`HostYield`: [`park`](sched::park),
/// [`wake`](sched::wake), [`yielding`](sched::yielding)).
pub mod sched {
    pub use crate::scheduler::{
        DeferCondition, Owner, SettleHook, WakeMode, YieldToken, advance, cancel_park,
        current_owner, defer, defer_owned, is_parked, is_scheduled, now, park, parked_labels,
        parked_waits, resume_due_now as resume_due, set_settle_hook, settle, sleeping_count, spawn,
        spawn_owned, stop_owner, wake, wake_error, yielding,
    };
}

pub use api::{DEFINITIONS_PATH, definitions, write_definitions};
pub use budget::untimed;
pub use compile::compile_check;
pub use diag::{ErrorKind, ScriptError};
pub use host::{CommandHost, NoServices, ScriptHost, WorldHost};
pub use modules::Reload;
pub use runtime::{Context, ReloadReport, RuntimeConfig, ScriptRuntime};
pub use vm::{ContextKind, OutputLine};

/// An mlua integer as an `i64`.
///
/// `mlua::Integer` is `i64` on 64-bit targets and `i32` on wasm32 (ADR-0049), so code that
/// reads one converts through this to compile on both.
#[allow(
    clippy::useless_conversion,
    reason = "mlua::Integer is i64 here but i32 on wasm32"
)]
pub fn lua_int(i: mlua::Integer) -> i64 {
    i64::from(i)
}

use re_core::DiagInfo;

/// Every diagnostic code `re_script` owns, with its title and explain page.
pub const DIAGNOSTICS: &[DiagInfo] = &[
    DiagInfo {
        code: codes::SYNTAX,
        title: "Luau syntax error",
        page: include_str!("../explain/RE3001.md"),
    },
    DiagInfo {
        code: codes::RUNTIME,
        title: "Script runtime error",
        page: include_str!("../explain/RE3002.md"),
    },
    DiagInfo {
        code: codes::SCRIPT_READ,
        title: "Cannot read script",
        page: include_str!("../explain/RE3003.md"),
    },
    DiagInfo {
        code: codes::REQUIRE,
        title: "Require refused or not found",
        page: include_str!("../explain/RE3004.md"),
    },
    DiagInfo {
        code: codes::YIELD,
        title: "task.wait cannot yield here",
        page: include_str!("../explain/RE3005.md"),
    },
    DiagInfo {
        code: codes::BAD_ARGUMENT,
        title: "Bad argument to an engine function",
        page: include_str!("../explain/RE3006.md"),
    },
    DiagInfo {
        code: codes::STALE_HANDLE,
        title: "Entity handle is stale",
        page: include_str!("../explain/RE3007.md"),
    },
    DiagInfo {
        code: codes::COMPONENT_DEFINITION,
        title: "Invalid component definition",
        page: include_str!("../explain/RE3008.md"),
    },
    DiagInfo {
        code: codes::COMMANDS_UNAVAILABLE,
        title: "Commands unavailable",
        page: include_str!("../explain/RE3009.md"),
    },
    DiagInfo {
        code: codes::TIMEOUT,
        title: "Script exceeded its CPU budget",
        page: include_str!("../explain/RE3010.md"),
    },
    DiagInfo {
        code: codes::MEMORY,
        title: "Script memory limit reached",
        page: include_str!("../explain/RE3011.md"),
    },
    DiagInfo {
        code: codes::GRAPH_BUILDER,
        title: "Invalid ProcGraph builder",
        page: include_str!("../explain/RE3012.md"),
    },
    DiagInfo {
        code: codes::DEFER_LIMIT,
        title: "Too many deferred threads",
        page: include_str!("../explain/RE3014.md"),
    },
    DiagInfo {
        code: codes::SERVICE_UNAVAILABLE,
        title: "Engine service unavailable",
        page: include_str!("../explain/RE3015.md"),
    },
    DiagInfo {
        code: debug::NOT_PAUSED,
        title: "Breakpoint reached where the script cannot pause",
        page: include_str!("../explain/RE3070.md"),
    },
    DiagInfo {
        code: debug::DAP,
        title: "Debugger DAP server failed",
        page: include_str!("../explain/RE3071.md"),
    },
    DiagInfo {
        code: codes::UNSUPPORTED_MEMBER,
        title: "Unsupported API member",
        page: include_str!("../explain/RE9001.md"),
    },
];

#[cfg(test)]
mod catalog {
    #[test]
    fn diagnostics_catalog_is_valid() {
        // re_script owns two blocks; check each against its own range. Of the
        // compatibility block it keeps RE9001 only (docs/M2_PLAN.md).
        let (script, compat): (Vec<_>, Vec<_>) = super::DIAGNOSTICS
            .iter()
            .copied()
            .partition(|d| d.code.number() < 9000);
        let mut problems = re_core::validate_catalog(&script, 3000..=3079);
        problems.extend(re_core::validate_catalog(&compat, 9001..=9001));
        assert!(problems.is_empty(), "{problems:#?}");
    }
}
