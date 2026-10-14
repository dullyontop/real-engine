//! The diagnostic codes `re_command` owns (range `RE2000`–`RE2999`).
//!
//! Each code has an explain page in `crates/re_command/explain/` and an entry in
//! [`DIAGNOSTICS`](crate::DIAGNOSTICS). A code is never reused for a different
//! meaning (ADR-0004).

use re_core::Code;

/// An operation of a transaction failed, so the whole transaction was rolled
/// back (the message names the operation's zero-based index; the cause is a
/// related diagnostic).
pub const TRANSACTION_REJECTED: Code = Code::new("RE2001");

/// `add_component` on an entity that already has the component.
pub const COMPONENT_ALREADY_PRESENT: Code = Code::new("RE2002");

/// A structural edit (reparent, rename, add or remove a component, an authored
/// child) on a generated entity.
pub const GENERATED_ENTITY: Code = Code::new("RE2003");

/// No command of this name is registered.
pub const UNKNOWN_COMMAND: Code = Code::new("RE2004");

/// A command's arguments or an operation do not match its input schema.
pub const INVALID_ARGUMENTS: Code = Code::new("RE2005");

/// The engine side of a `remote::channel` is gone.
pub const ENGINE_UNAVAILABLE: Code = Code::new("RE2006");

/// An undo or redo, or the rollback of a rejected transaction or a dry run,
/// could not be applied (the world changed underneath the history).
pub const UNDO_FAILED: Code = Code::new("RE2007");
