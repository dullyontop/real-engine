//! `re_agent`: agent tooling for Real Engine (ADR-0020, ADR-0026, ADR-0027).
//!
//! * [`lock`]: the single-writer project lock (`.real/editor.lock`), used by
//!   every writer (`real do`, `real mcp`, `real editor`); code `RE8003`;
//! * [`remote`]: the editor socket (`.real/editor.sock`): the protocol, the
//!   server loop the editor hosts and the client that other writers use.
//!
//! `lock` and `remote` are frozen contracts from WP7's first PR
//! (docs/M1_PLAN.md, "Cross-WP contracts").
//!
//! This is a tooling crate: `re_cli` links it only with its `tooling` feature,
//! and it never depends on tokio.

#![warn(missing_docs)]

pub mod codes;
pub mod lock;
pub mod remote;

pub use lock::{Holder, LockState, ProjectLock, acquire, probe};

use re_core::DiagInfo;

/// Every diagnostic code `re_agent` owns, with its title and explain page.
pub const DIAGNOSTICS: &[DiagInfo] = &[
    DiagInfo {
        code: codes::PROJECT_LOCKED,
        title: "Project locked by another writer",
        page: include_str!("../explain/RE8003.md"),
    },
    DiagInfo {
        code: codes::SOCKET_UNSUPPORTED,
        title: "Editor socket not available on this platform",
        page: include_str!("../explain/RE8004.md"),
    },
    DiagInfo {
        code: codes::SOCKET_IO,
        title: "Editor socket connection failed",
        page: include_str!("../explain/RE8005.md"),
    },
    DiagInfo {
        code: codes::SOCKET_PROTOCOL,
        title: "Invalid editor socket message",
        page: include_str!("../explain/RE8006.md"),
    },
    DiagInfo {
        code: codes::LOCK_IO,
        title: "Project lock file error",
        page: include_str!("../explain/RE8007.md"),
    },
];

/// The code range this crate owns (ARCHITECTURE §7).
pub const CODE_RANGE: std::ops::RangeInclusive<u16> = 8000..=8099;
