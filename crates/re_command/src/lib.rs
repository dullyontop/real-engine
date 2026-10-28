//! `re_command`: the model layer of Real Engine (ADR-0003).
//!
//! Every change to a scene, from the editor, the CLI, Luau tools or an agent,
//! goes through this crate:
//!
//! * [`op`]: [`Op`], the closed set of edit operations, each with its exact
//!   inverse (scene edits, and `set_doc` on open documents, ADR-0044);
//! * [`history`]: [`Transaction`] (atomic), [`History`] (undo, redo, dry runs)
//!   and the append-only log of [`LogEntry`] lines with their [`Origin`];
//! * [`registry`]: [`CommandRegistry`], named commands with JSON Schema
//!   inputs, a [`Tier`], the files they name ([`PathArg`]), dry runs and the
//!   [`PolicyCheck`] hook;
//! * [`builtins`]: the built-in commands (`describe`, `query`, `entity.get`,
//!   `command.describe`, `project.status`, `entity.spawn`, `entity.set`,
//!   `entity.reparent`, `entity.delete`, `component.add`, `component.remove`,
//!   `scene.apply`, `undo`, `redo`);
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
pub use history::{History, LogEntry, LogKind, Origin, Stacks, Transaction};
pub use op::{Op, apply};
pub use registry::{
    AllowAll, Call, CommandAnnotations, CommandOutcome, CommandRegistry, CommandSpec, EditState,
    EditTarget, Handler, NoArgs, PathAccess, PathArg, PolicyCheck, Tier, declared_paths,
    input_schema,
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
    DiagInfo {
        code: codes::INVALID_DOCUMENT,
        title: "Document not valid",
        page: include_str!("../explain/RE2008.md"),
    },
    DiagInfo {
        code: codes::DOCUMENT_FILE_KEPT,
        title: "Document opened from a file cannot be emptied",
        page: include_str!("../explain/RE2050.md"),
    },
    DiagInfo {
        code: codes::DOCUMENT_EXISTS,
        title: "The file or name exists",
        page: include_str!("../explain/RE2051.md"),
    },
    DiagInfo {
        code: codes::DOCUMENT_PATH,
        title: "Not a path of the document's kind",
        page: include_str!("../explain/RE2052.md"),
    },
    DiagInfo {
        code: codes::NAMED_FILE_MISSING,
        title: "A file the document names does not exist",
        page: include_str!("../explain/RE2054.md"),
    },
    DiagInfo {
        code: codes::WORLD_RULE,
        title: "Transaction breaks the rule of what is being edited",
        page: include_str!("../explain/RE2056.md"),
    },
    DiagInfo {
        code: codes::CONFIG_NOT_TOML,
        title: "A configuration key or value real.toml cannot hold",
        page: include_str!("../explain/RE2057.md"),
    },
];

/// The code range this crate owns (ARCHITECTURE §7).
pub const CODE_RANGE: std::ops::RangeInclusive<u16> = 2000..=2999;
