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
//!
//! Diagnostic codes: `RE3000`–`RE3079` and the compatibility block `RE9xxx`
//! (ARCHITECTURE §7); see [`codes`].

#![warn(missing_docs)]

pub mod codes;
mod compile;
pub mod diag;

pub use compile::compile_check;
pub use diag::{ErrorKind, ScriptError};

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
        code: codes::TIMEOUT,
        title: "Script exceeded its CPU budget",
        page: include_str!("../explain/RE3010.md"),
    },
    DiagInfo {
        code: codes::MEMORY,
        title: "Script memory limit reached",
        page: include_str!("../explain/RE3011.md"),
    },
];

#[cfg(test)]
mod catalog {
    #[test]
    fn diagnostics_catalog_is_valid() {
        let mut problems = re_core::validate_catalog(super::DIAGNOSTICS, 3000..=3079);
        problems.retain(|p| !p.contains("RE9"));
        assert!(problems.is_empty(), "{problems:#?}");
    }
}
