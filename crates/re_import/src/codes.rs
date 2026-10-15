//! The diagnostic codes `re_import` owns (range `RE9800`–`RE9899`).
//!
//! Each code has an explain page in `crates/re_import/explain/` and an entry in
//! [`DIAGNOSTICS`](crate::DIAGNOSTICS). A code is never reused for a different
//! meaning (ADR-0004).

use re_core::Code;

/// The input cannot be read, or is neither a Rojo-style project nor a Luau
/// loader script.
pub const INPUT_UNREADABLE: Code = Code::new("RE9801");

/// The loader script does not parse as Luau.
pub const LOADER_SYNTAX: Code = Code::new("RE9802");

/// The loader has a statement the importer does not recognise. Outside the
/// instance-building blocks it is skipped and reported (a warning); inside
/// one it stops the import (an error), because skipping it could build the
/// wrong tree.
pub const UNRECOGNISED_STATEMENT: Code = Code::new("RE9803");

/// The target project cannot take an import: it is not a compat project, or
/// its `real.toml` or entry scene cannot be read.
pub const PROJECT_UNSUITABLE: Code = Code::new("RE9804");

/// One instance of the input could not be imported (an unknown service or
/// class, a property the class layer refuses, a name clash with an instance
/// the importer did not create). The rest of the input is imported.
pub const INSTANCE_FAILED: Code = Code::new("RE9805");

/// A Rojo-style project file or one of its `*.model.json` / `*.meta.json`
/// files is malformed.
pub const ROJO_INVALID: Code = Code::new("RE9806");

/// An imported script does not compile. It is imported anyway, so it can be
/// fixed in the project.
pub const SCRIPT_SYNTAX: Code = Code::new("RE9807");

/// Writing the project failed part-way.
pub const WRITE_FAILED: Code = Code::new("RE9808");
