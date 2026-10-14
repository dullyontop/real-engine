//! The diagnostic codes `re_scene` owns (range `RE1200`–`RE1999`).
//!
//! Each code has an explain page in `crates/re_scene/explain/` and an entry in
//! [`DIAGNOSTICS`](crate::DIAGNOSTICS). A code is never reused for a different
//! meaning (ADR-0004).

use re_core::Code;

/// No entity has this stable id.
pub const UNKNOWN_ID: Code = Code::new("RE1200");

/// An entity with this stable id already exists.
pub const DUPLICATE_ID: Code = Code::new("RE1201");

/// A stable id is empty, or contains `/` (reserved for generated ids).
pub const INVALID_ID: Code = Code::new("RE1202");

/// A reparent would make an entity its own ancestor.
pub const HIERARCHY_CYCLE: Code = Code::new("RE1203");

/// A scene or prefab file does not parse, or an entity in it is invalid
/// (the message names the entity index, id, component and field).
pub const PARSE_ERROR: Code = Code::new("RE1204");

/// An entity's `parent` is not defined earlier in the file or in the world.
pub const FORWARD_PARENT: Code = Code::new("RE1205");

/// A file has the wrong `format` or an unsupported `version`.
pub const UNSUPPORTED_FORMAT: Code = Code::new("RE1206");

/// A procedural source's generator failed (or is unknown, or returned an
/// invalid fragment); the previous output stays in place.
pub const GENERATOR_FAILED: Code = Code::new("RE1207");

/// A file's `script_components` entry does not match the component already registered.
pub const SCRIPT_COMPONENT_MISMATCH: Code = Code::new("RE1208");

/// An override of a generated entity could not be applied after regeneration (warning).
pub const OVERRIDE_NOT_APPLIED: Code = Code::new("RE1209");

/// The entity is not a generated entity, so it cannot be suppressed.
pub const NOT_GENERATED: Code = Code::new("RE1210");

/// Scene content that is well-formed but not supported (for example a
/// procedural source without a stable id, or an entity reference inside a prefab).
pub const INVALID_CONTENT: Code = Code::new("RE1211");
