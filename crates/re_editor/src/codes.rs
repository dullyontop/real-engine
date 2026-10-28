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

/// The clipboard does not hold entities the editor can paste: it holds other
/// text, entities in another version of the format, or subtrees that are not
/// whole. Nothing is pasted.
pub const NOT_ENTITIES: Code = Code::new("RE8805");

/// A paste was refused: a copied entity has a component this project does
/// not define. Nothing is pasted.
pub const PASTE_UNKNOWN_COMPONENT: Code = Code::new("RE8806");

/// A generated entity was left out of a copy: it shows a derived asset,
/// which exists only while its procedural source makes it.
pub const COPY_LEFT_OUT: Code = Code::new("RE8807");

/// A pasted entity referred to an entity that does not exist where it was
/// pasted; the reference was cleared.
pub const REFERENCE_CLEARED: Code = Code::new("RE8808");

/// A node parameter's value (on the canvas, or an override in the Procedural
/// panel) does not fit its schema: text typed into its JSON box does not
/// parse or is not of its type, so nothing was changed; or the value in the
/// file is not of its type, so it is shown as JSON text.
pub const PARAM_VALUE: Code = Code::new("RE8850");

/// A builder script makes the graph shown on the node canvas, so the canvas
/// shows it read-only (a note, once per source).
pub const BUILDER_READ_ONLY: Code = Code::new("RE8852");

/// An asset's thumbnail in the Assets panel could not be made (a warning,
/// once until the file changes): the file cannot be read or decoded, or a
/// material does not load or cannot be drawn. Its kind's icon is shown.
pub const THUMBNAIL: Code = Code::new("RE8853");
