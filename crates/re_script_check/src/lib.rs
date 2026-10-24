//! `re_script_check`: script validation tooling (ADR-0013, ADR-0014, ADR-0024).
//!
//! * [`scan`](mod@scan): the static API scan with full_moon (tier 2), giving an
//!   [`ApiUsage`], and [`GapReport`] against the native coverage table.
//! * [`types::run`]: the external Luau type checker (tier 3), luau-lsp
//!   [`types::LUAU_LSP_VERSION`] by default.
//! * [`corpus::run`]: the corpus suite over directories of scripts, with
//!   compile (tier 1, [`re_script::compile_check`]), scan and optional types, as
//!   a [`corpus::CorpusReport`].
//! * [`pow_warnings`]: `RE3083` for `^` with an exponent that calls the
//!   platform's `pow` (D2 determinism, ADR-0036).
//! * [`roblox::definitions`]: `roblox.d.luau`, the compat API's Luau types,
//!   generated from `re_compat`'s tables for `real check --types` in a compat
//!   project.
//!
//! `scan`, `types::run` and `corpus::run` are **frozen** at WP3's exit; `re_cli`
//! (`real check`, `real verify`) calls them. This is a tooling crate: it links
//! full_moon (MPL-2.0), so the runtime build never reaches it.
//!
//! Diagnostic codes: `RE3080`–`RE3099`; see [`codes`].

#![warn(missing_docs)]

pub mod codes;
pub mod corpus;
pub mod gap;
pub mod portable;
pub mod roblox;
pub mod scan;
pub mod types;

pub use gap::{Coverage, GapReport};
pub use portable::pow_warnings;
pub use scan::{ApiUsage, scan, scan_source};

use re_core::DiagInfo;

/// Every diagnostic code `re_script_check` owns, with its title and explain page.
pub const DIAGNOSTICS: &[DiagInfo] = &[
    DiagInfo {
        code: codes::SCAN_PARSE,
        title: "Static scan cannot parse the file",
        page: include_str!("../explain/RE3080.md"),
    },
    DiagInfo {
        code: codes::TYPE_ERROR,
        title: "Type error",
        page: include_str!("../explain/RE3081.md"),
    },
    DiagInfo {
        code: codes::TYPE_WARNING,
        title: "Type checker warning",
        page: include_str!("../explain/RE3082.md"),
    },
    DiagInfo {
        code: codes::PLATFORM_POW,
        title: "`^` calls the platform's pow",
        page: include_str!("../explain/RE3083.md"),
    },
    DiagInfo {
        code: codes::CHECKER_NOT_FOUND,
        title: "Type checker not found",
        page: include_str!("../explain/RE3090.md"),
    },
    DiagInfo {
        code: codes::CHECKER_FAILED,
        title: "Type checker failed",
        page: include_str!("../explain/RE3091.md"),
    },
];

#[cfg(test)]
mod catalog {
    #[test]
    fn diagnostics_catalog_is_valid() {
        let problems = re_core::validate_catalog(super::DIAGNOSTICS, 3080..=3099);
        assert!(problems.is_empty(), "{problems:#?}");
    }
}
