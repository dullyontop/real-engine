//! The diagnostic codes `re_geom` owns (`RE4500`–`RE4599`, inside the
//! `RE4xxx` block it shares with `re_proc`).
//!
//! Each code has an explain page in `crates/re_geom/explain/` and an entry in
//! [`DIAGNOSTICS`](crate::DIAGNOSTICS). A code is never reused for a different
//! meaning (ADR-0004).

use re_core::Code;

/// A heightfield has fewer than 2 samples per side, the wrong number of
/// samples, a non-positive spacing, or a non-finite value.
pub const INVALID_HEIGHTFIELD: Code = Code::new("RE4500");

/// A primitive's size or tessellation is out of range.
pub const INVALID_PRIMITIVE: Code = Code::new("RE4501");

/// A merged mesh would have more vertices than a `u32` index can name.
pub const MESH_TOO_LARGE: Code = Code::new("RE4502");

/// A coordinate is too far from the origin for the 1 mm grid of polygon
/// operations (about ±1,000 km).
pub const OUT_OF_RANGE: Code = Code::new("RE4503");

/// A polygon or path is unusable: too few points, a non-finite point, or a
/// non-positive width.
pub const INVALID_POLYGON: Code = Code::new("RE4504");

/// A polygon could not be triangulated.
pub const TRIANGULATION_FAILED: Code = Code::new("RE4505");

/// An argument to a modelling operation (`poly`) is out of range or not
/// finite, or names an attribute or group that cannot be used.
pub const INVALID_ARGUMENT: Code = Code::new("RE4510");

/// A [`PolyMesh`](crate::poly::PolyMesh) breaks one of the rules of
/// [`PolyMesh::validate`](crate::poly::PolyMesh::validate).
pub const INVALID_POLYMESH: Code = Code::new("RE4511");

/// A modelling operation's result would have more than
/// [`MAX_ELEMENTS`](crate::poly::MAX_ELEMENTS) points or corners.
pub const POLYMESH_TOO_LARGE: Code = Code::new("RE4515");
