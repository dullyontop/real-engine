//! The diagnostic codes `re_assets` owns (range `RE7000`–`RE7199`).
//!
//! Each code has an explain page in `crates/re_assets/explain/` and an entry in
//! [`DIAGNOSTICS`](crate::DIAGNOSTICS). A code is never reused for a different
//! meaning (ADR-0004).

use re_core::Code;

/// A source file cannot be read.
pub const READ_FAILED: Code = Code::new("RE7001");

/// A glTF file is not valid glTF 2.0, or its buffers cannot be loaded.
pub const GLTF_INVALID: Code = Code::new("RE7002");

/// An image cannot be decoded, or its format is not supported.
pub const IMAGE_INVALID: Code = Code::new("RE7003");

/// A glTF mesh primitive has no `POSITION` attribute.
pub const MISSING_POSITIONS: Code = Code::new("RE7004");

/// A glTF part was skipped: a non-triangle primitive, morph-target weights, or
/// an index past the end of the vertices (a warning).
pub const GLTF_PART_SKIPPED: Code = Code::new("RE7005");

/// A material file is not valid material JSON.
pub const MATERIAL_INVALID: Code = Code::new("RE7010");

/// A material names a texture file that does not exist.
pub const MATERIAL_TEXTURE_MISSING: Code = Code::new("RE7011");

/// A material value is out of range (for example roughness above 1).
pub const MATERIAL_VALUE_OUT_OF_RANGE: Code = Code::new("RE7012");

/// A `.meta.json` sidecar is not valid.
pub const META_INVALID: Code = Code::new("RE7020");

/// A file (sidecar or cache entry) cannot be written.
pub const WRITE_FAILED: Code = Code::new("RE7021");

/// The file watcher cannot start.
pub const WATCH_FAILED: Code = Code::new("RE7030");

/// An asset path is not a valid project-relative path.
pub const PATH_INVALID: Code = Code::new("RE7040");

/// An asset id or clip reference does not name a loaded asset.
pub const UNKNOWN_ASSET: Code = Code::new("RE7041");

/// The file extension has no importer.
pub const UNSUPPORTED_ASSET: Code = Code::new("RE7042");

/// A changed asset failed to re-import; the last good version stays loaded (a warning).
pub const RELOAD_FAILED: Code = Code::new("RE7050");

/// A glTF skin or its skin weights are malformed (ADR-0039).
pub const SKIN_INVALID: Code = Code::new("RE7060");

/// Some vertices have more than four joint influences; each keeps its four
/// largest (a warning).
pub const SKIN_TOO_MANY_INFLUENCES: Code = Code::new("RE7061");

/// Several nodes share a joint's name, so the joints are renamed (a warning).
pub const SKIN_DUPLICATE_JOINT_NAME: Code = Code::new("RE7062");

/// A mesh on a skinned node has no skin weights and is drawn unskinned (a warning).
pub const SKIN_UNWEIGHTED_MESH: Code = Code::new("RE7063");
