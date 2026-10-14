//! `re_command`: the model layer of Real Engine (ADR-0003).
//!
//! Every change to a scene, from the editor, the CLI, Luau tools or an agent,
//! goes through this crate:
//!
//! * [`op`]: [`Op`], the closed set of edit operations, each with its exact
//!   inverse;
//! * [`history`]: [`Transaction`] (atomic), [`History`] (undo, redo, dry runs)
//!   and the append-only log of [`LogEntry`] lines with their [`Origin`];
//! * [`registry`]: [`CommandRegistry`], named commands with JSON Schema
//!   inputs, a [`Tier`], dry runs and the [`PolicyCheck`] hook;
//! * [`builtins`]: the built-in commands (`describe`, `query`, `entity.get`,
//!   `command.describe`, `entity.spawn`, `entity.set`, `entity.reparent`,
//!   `entity.delete`, `component.add`, `component.remove`, `scene.apply`,
//!   `undo`, `redo`);
//! * [`remote`]: the std channel that carries command calls from other
//!   threads to the thread that owns the world;
//! * [`error`]: [`OpError`] and [`TxError`], with codes `RE2000`–`RE2999`.
//!
//! This is a frozen contract from WP0 PR "entity core" (docs/M1_PLAN.md).

#![warn(missing_docs)]

pub mod builtins;
pub mod codes;
pub mod error;
pub mod history;
pub mod op;
pub mod registry;
pub mod remote;

pub use builtins::register_builtin_commands;
pub use error::{OpError, TxError};
pub use history::{History, LogEntry, LogKind, Origin, Transaction};
pub use op::{Op, apply};
pub use registry::{
    AllowAll, Call, CommandAnnotations, CommandOutcome, CommandRegistry, CommandSpec, EditState,
    EditTarget, Handler, NoArgs, PolicyCheck, Tier, input_schema,
};
pub use remote::{EngineHandle, EngineReceiver, EngineRequest, channel};

use re_core::DiagInfo;

/// Every diagnostic code `re_command` owns, with its title and explain page.
pub const DIAGNOSTICS: &[DiagInfo] = &[
    DiagInfo {
        code: codes::TRANSACTION_REJECTED,
        title: "Transaction rejected",
        page: include_str!("../explain/RE2001.md"),
    },
    DiagInfo {
        code: codes::COMPONENT_ALREADY_PRESENT,
        title: "Component already present",
        page: include_str!("../explain/RE2002.md"),
    },
    DiagInfo {
        code: codes::GENERATED_ENTITY,
        title: "Structural edit of a generated entity",
        page: include_str!("../explain/RE2003.md"),
    },
    DiagInfo {
        code: codes::UNKNOWN_COMMAND,
        title: "Unknown command",
        page: include_str!("../explain/RE2004.md"),
    },
    DiagInfo {
        code: codes::INVALID_ARGUMENTS,
        title: "Invalid command arguments",
        page: include_str!("../explain/RE2005.md"),
    },
    DiagInfo {
        code: codes::ENGINE_UNAVAILABLE,
        title: "Engine unavailable",
        page: include_str!("../explain/RE2006.md"),
    },
    DiagInfo {
        code: codes::UNDO_FAILED,
        title: "Undo or redo failed",
        page: include_str!("../explain/RE2007.md"),
    },
];

/// The code range this crate owns (ARCHITECTURE §7).
pub const CODE_RANGE: std::ops::RangeInclusive<u16> = 2000..=2999;
