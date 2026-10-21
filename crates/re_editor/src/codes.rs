//! The diagnostic codes `re_editor` owns (range `RE8800`–`RE8899`).
//!
//! Each code has an explain page in `crates/re_editor/explain/` and an entry in
//! [`DIAGNOSTICS`](crate::DIAGNOSTICS). A code is never reused for a different
//! meaning (ADR-0004).

use re_core::Code;

/// The editor's window, event loop or window surface could not be created
/// (usually: no display).
pub const NO_DISPLAY: Code = Code::new("RE8801");

/// The command palette's arguments are not a JSON object.
pub const PALETTE_ARGS: Code = Code::new("RE8802");

/// Text typed into an inspector field (an entity id, a graph parameter) could
/// not be read as a value.
pub const INVALID_FIELD_TEXT: Code = Code::new("RE8803");

/// The editor layout file (`.real/editor/layout.json`) could not be read: it
/// does not parse, names a panel this editor does not know, or has another
/// version. The default layout is used.
pub const LAYOUT_UNREADABLE: Code = Code::new("RE8804");
