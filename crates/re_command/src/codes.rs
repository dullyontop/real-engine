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
/// could not be applied (the world changed underneath the history, or an
/// open document it edited was reopened).
pub const UNDO_FAILED: Code = Code::new("RE2007");

/// A transaction left an open document in a state its kind's validator
/// refuses (ADR-0044), so the transaction was rolled back.
pub const INVALID_DOCUMENT: Code = Code::new("RE2008");

/// A `set_doc` would empty an open document that was opened from an existing
/// file: a save removes only files the session made (ADR-0064).
pub const DOCUMENT_FILE_KEPT: Code = Code::new("RE2050");

/// A document command would make a file, or a name, that exists already
/// (ADR-0064).
pub const DOCUMENT_EXISTS: Code = Code::new("RE2051");

/// A document command was given a path that is not of its kind (such as a
/// material path not ending in `.material.json`), or outside the project
/// (ADR-0064).
pub const DOCUMENT_PATH: Code = Code::new("RE2052");

/// A document names a file that does not exist, such as a material's texture
/// (ADR-0064).
pub const NAMED_FILE_MISSING: Code = Code::new("RE2054");

/// A transaction would leave the World breaking the rule of what is being
/// edited, such as one root and no `Procedural` while a prefab is open
/// (ADR-0064), so it was rolled back.
pub const WORLD_RULE: Code = Code::new("RE2056");

/// A configuration key or value that `real.toml` cannot hold: a key that is
/// not a dotted path of bare TOML keys and array indices, or a `null`
/// (ADR-0064).
pub const CONFIG_NOT_TOML: Code = Code::new("RE2057");
